use crate::gitlab::{Job, PipelineGraph};
use crate::pipeline::{ArtifactManager, CacheManager, CacheMountSpec};
use anyhow::Result;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone)]
pub struct VolumeMount {
    pub host: PathBuf,
    pub container: PathBuf,
    pub read_only: bool,
}

pub fn collect_volume_mounts(
    job: &Job,
    graph: &PipelineGraph,
    artifacts: &ArtifactManager,
    cache: &CacheManager,
    cache_env: &HashMap<String, String>,
    container_root: &Path,
) -> Result<Vec<VolumeMount>> {
    let mut collector = MountCollector::new(container_root);

    for (host, relative) in artifacts.job_mount_specs(job) {
        let container = collector.container_path(&relative);
        collector.push(host, container, false);
    }

    for dependency in &job.needs {
        if !dependency.needs_artifacts {
            continue;
        }
        let dep_job = graph
            .graph
            .node_weights()
            .find(|d| d.name == dependency.job);
        if dep_job.is_none() {
            warn!(
                job = dependency.job,
                "dependency not present in pipeline graph"
            );
            continue;
        }
        for (host, relative) in artifacts.dependency_mount_specs(&dependency.job, dep_job) {
            let container = collector.container_path(&relative);
            collector.push(host, container, true);
        }
    }

    let cache_specs = cache.mount_specs(&job.cache, cache_env)?;
    for CacheMountSpec {
        host,
        relative,
        read_only,
    } in cache_specs
    {
        let container = collector.container_path(&relative);
        collector.push(host, container, read_only);
    }

    Ok(collector.into_mounts())
}

impl VolumeMount {
    pub fn to_arg(&self) -> OsString {
        let mut arg = OsString::new();
        arg.push(self.host.as_os_str());
        arg.push(":");
        arg.push(self.container.as_os_str());
        if self.read_only {
            arg.push(":ro");
        }
        arg
    }
}

struct MountCollector<'a> {
    container_root: &'a Path,
    mounts: Vec<VolumeMount>,
}

impl<'a> MountCollector<'a> {
    fn new(container_root: &'a Path) -> Self {
        Self {
            container_root,
            mounts: Vec::new(),
        }
    }

    fn push(&mut self, host: PathBuf, container: PathBuf, read_only: bool) {
        if self
            .mounts
            .iter()
            .any(|existing| existing.host == host && existing.container == container)
        {
            return;
        }
        self.mounts.push(VolumeMount {
            host,
            container,
            read_only,
        });
    }

    fn container_path(&self, relative: &Path) -> PathBuf {
        if relative.is_absolute() {
            relative.to_path_buf()
        } else {
            self.container_root.join(relative)
        }
    }

    fn into_mounts(self) -> Vec<VolumeMount> {
        self.mounts
    }
}
