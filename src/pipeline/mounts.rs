use crate::model::{DependencySourceSpec, JobSpec, PipelineSpec};
use crate::pipeline::{
    ArtifactManager, CacheManager, CacheMountSpec, artifacts::ExternalArtifactsManager,
    planner::JobPlan,
};
use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone)]
pub struct VolumeMount {
    pub host: PathBuf,
    pub container: PathBuf,
    pub read_only: bool,
}

pub struct VolumeMountContext<'a> {
    pub job: &'a JobSpec,
    pub plan: &'a JobPlan,
    pub pipeline: &'a PipelineSpec,
    pub artifacts: &'a ArtifactManager,
    pub cache: &'a CacheManager,
    pub cache_env: &'a HashMap<String, String>,
    pub container_root: &'a Path,
    pub external: Option<&'a ExternalArtifactsManager>,
}

fn mount_external_artifacts(root: &Path, collector: &mut MountCollector<'_>) -> Result<()> {
    if !root.exists() {
        return Err(anyhow!(
            "external artifact directory {} does not exist",
            root.display()
        ));
    }
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let rel = match path.strip_prefix(root) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
            _ => continue,
        };
        let container = collector.container_path(&rel);
        collector.push(path, container, true);
    }
    Ok(())
}

pub fn collect_volume_mounts(ctx: VolumeMountContext<'_>) -> Result<Vec<VolumeMount>> {
    let VolumeMountContext {
        job,
        plan,
        pipeline,
        artifacts,
        cache,
        cache_env,
        container_root,
        external,
    } = ctx;
    let mut collector = MountCollector::new(container_root);

    for (host, relative) in artifacts.job_mount_specs(job) {
        let container = collector.container_path(&relative);
        collector.push(host, container, false);
    }

    for dependency in &job.needs {
        if !dependency.needs_artifacts {
            continue;
        }
        match &dependency.source {
            DependencySourceSpec::Local => {
                let dep_job = pipeline.jobs.get(&dependency.job).cloned();
                let Some(dep_job) = dep_job else {
                    if dependency.optional {
                        continue;
                    }
                    return Err(anyhow!(
                        "job '{}' depends on unknown job '{}'",
                        job.name,
                        dependency.job
                    ));
                };
                let variant_names = plan.variants_for_dependency(dependency);
                if variant_names.is_empty() {
                    if dependency.optional {
                        continue;
                    }
                    return Err(anyhow!(
                        "job '{}' requires artifacts from '{}', but it did not run",
                        job.name,
                        dependency.job
                    ));
                }
                for variant in variant_names {
                    for (host, relative) in artifacts.dependency_mount_specs(
                        &variant,
                        Some(&dep_job),
                        dependency.optional,
                    ) {
                        let container = collector.container_path(&relative);
                        collector.push(host, container, true);
                    }
                }
            }
            DependencySourceSpec::External(ext) => {
                let Some(manager) = external else {
                    if dependency.optional {
                        warn!(
                            job = job.name,
                            dependency = %dependency.job,
                            "skipping cross-project dependency (GitLab credentials not configured)"
                        );
                        continue;
                    } else {
                        return Err(anyhow!(
                            "job '{}' requires artifacts from project '{}' but no GitLab token is configured",
                            job.name,
                            ext.project
                        ));
                    }
                };
                match manager.ensure_artifacts(&ext.project, &dependency.job, &ext.reference) {
                    Ok(root) => {
                        mount_external_artifacts(&root, &mut collector)?;
                    }
                    Err(err) => {
                        if dependency.optional {
                            warn!(
                                job = job.name,
                                dependency = %dependency.job,
                                project = %ext.project,
                                "failed to download dependency artifacts: {err}"
                            );
                            continue;
                        } else {
                            return Err(err.context(format!(
                                "failed to download artifacts for '{}' from project '{}'",
                                dependency.job, ext.project
                            )));
                        }
                    }
                }
            }
        }
    }

    for dep_name in &job.dependencies {
        if let Some(dep_planned) = plan.nodes.get(dep_name) {
            for relative in &dep_planned.job.artifacts.paths {
                let host = artifacts.job_artifact_host_path(&dep_planned.job.name, relative);
                if !host.exists() {
                    warn!(job = dep_planned.job.name, path = %relative.display(), "artifact missing");
                    continue;
                }
                let container = collector.container_path(relative);
                collector.push(host, container, true);
            }
            continue;
        }
        let dep_job = pipeline.jobs.get(dep_name);
        let Some(dep_job) = dep_job else {
            warn!(job = dep_name, "dependency not present in pipeline graph");
            continue;
        };
        for relative in &dep_job.artifacts.paths {
            let host = artifacts.job_artifact_host_path(&dep_job.name, relative);
            if !host.exists() {
                warn!(job = dep_name, path = %relative.display(), "artifact missing");
                continue;
            }
            let container = collector.container_path(relative);
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
