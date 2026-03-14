use crate::naming::job_name_slug;
use crate::gitlab::Job;
use anyhow::{Context, Result};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use tracing::warn;

#[derive(Debug, Clone)]
pub struct ArtifactManager {
    root: PathBuf,
}

impl ArtifactManager {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn prepare_targets(&self, job: &Job) -> Result<()> {
        if job.artifacts.is_empty() {
            return Ok(());
        }
        let root = self.job_artifacts_root(&job.name);
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to prepare artifacts for {}", job.name))?;

        for relative in &job.artifacts {
            let host = self.job_artifact_host_path(&job.name, relative);
            match artifact_kind(relative) {
                ArtifactPathKind::Directory => {
                    fs::create_dir_all(&host).with_context(|| {
                        format!("failed to prepare artifact directory {}", host.display())
                    })?;
                }
                ArtifactPathKind::File => {
                    if let Some(parent) = host.parent() {
                        fs::create_dir_all(parent).with_context(|| {
                            format!("failed to prepare artifact parent {}", parent.display())
                        })?;
                    }
                    if !host.exists() {
                        File::create(&host).with_context(|| {
                            format!("failed to create artifact file {}", host.display())
                        })?;
                    }
                }
            }
        }

        Ok(())
    }

    pub fn job_mount_specs(&self, job: &Job) -> Vec<(PathBuf, PathBuf)> {
        job.artifacts
            .iter()
            .map(|relative| {
                let host = self.job_artifact_host_path(&job.name, relative);
                (host, relative.clone())
            })
            .collect()
    }

    pub fn dependency_mount_specs(
        &self,
        job_name: &str,
        job: Option<&Job>,
    ) -> Vec<(PathBuf, PathBuf)> {
        let Some(dep_job) = job else {
            return Vec::new();
        };

        let mut specs = Vec::new();
        for relative in &dep_job.artifacts {
            let host = self.job_artifact_host_path(&dep_job.name, relative);
            if !host.exists() {
                warn!(job = job_name, path = %relative.display(), "artifact missing");
                continue;
            }
            specs.push((host, relative.clone()));
        }

        specs
    }

    pub fn job_artifact_host_path(&self, job_name: &str, artifact: &Path) -> PathBuf {
        self.job_artifacts_root(job_name)
            .join(artifact_relative_path(artifact))
    }

    fn job_artifacts_root(&self, job_name: &str) -> PathBuf {
        self.root.join(job_name_slug(job_name)).join("artifacts")
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ArtifactPathKind {
    File,
    Directory,
}

fn artifact_relative_path(artifact: &Path) -> PathBuf {
    use std::path::Component;

    let mut rel = PathBuf::new();
    for component in artifact.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::ParentDir => continue,
            Component::Prefix(prefix) => rel.push(prefix.as_os_str()),
            Component::Normal(seg) => rel.push(seg),
        }
    }

    if rel.as_os_str().is_empty() {
        rel.push("artifact");
    }
    rel
}

fn artifact_kind(path: &Path) -> ArtifactPathKind {
    if path.to_string_lossy().ends_with(std::path::MAIN_SEPARATOR) {
        return ArtifactPathKind::Directory;
    }

    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) if name.contains('.') => ArtifactPathKind::File,
        _ => ArtifactPathKind::Directory,
    }
}
