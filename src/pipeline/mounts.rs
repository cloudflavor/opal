use crate::execution_plan::ExecutionPlan;
use crate::model::{ArtifactSourceOutcome, DependencySourceSpec, JobSpec, PipelineSpec};
use crate::pipeline::{
    ArtifactManager, CacheManager, CacheMountSpec, artifacts::ExternalArtifactsManager,
};
use anyhow::{Context, Result, anyhow};
use globset::{Glob, GlobSet, GlobSetBuilder};
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
    pub plan: &'a ExecutionPlan,
    pub pipeline: &'a PipelineSpec,
    pub workspace_root: &'a Path,
    pub artifacts: &'a ArtifactManager,
    pub cache: &'a CacheManager,
    pub cache_env: &'a HashMap<String, String>,
    pub completed_jobs: &'a HashMap<String, ArtifactSourceOutcome>,
    pub session_dir: &'a Path,
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
        workspace_root,
        artifacts,
        cache,
        cache_env,
        completed_jobs,
        session_dir,
        container_root,
        external,
    } = ctx;
    let mut collector = MountCollector::new(container_root);
    let mut dependency_mounts = Vec::new();

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
                        completed_jobs.get(&variant).copied(),
                        dependency.optional,
                    ) {
                        dependency_mounts.push(DependencyMount {
                            host,
                            relative,
                            exclude: dep_job.artifacts.exclude.clone(),
                        });
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
            if !dep_planned
                .instance
                .job
                .artifacts
                .when
                .includes(completed_jobs.get(dep_name).copied())
            {
                continue;
            }
            for relative in &dep_planned.instance.job.artifacts.paths {
                let host =
                    artifacts.job_artifact_host_path(&dep_planned.instance.job.name, relative);
                if !host.exists() {
                    warn!(job = dep_planned.instance.job.name, path = %relative.display(), "artifact missing");
                    continue;
                }
                dependency_mounts.push(DependencyMount {
                    host,
                    relative: relative.clone(),
                    exclude: dep_planned.instance.job.artifacts.exclude.clone(),
                });
            }
            continue;
        }
        let dep_job = pipeline.jobs.get(dep_name);
        let Some(dep_job) = dep_job else {
            warn!(job = dep_name, "dependency not present in pipeline graph");
            continue;
        };
        if !dep_job
            .artifacts
            .when
            .includes(completed_jobs.get(dep_name).copied())
        {
            continue;
        }
        for relative in &dep_job.artifacts.paths {
            let host = artifacts.job_artifact_host_path(&dep_job.name, relative);
            if !host.exists() {
                warn!(job = dep_name, path = %relative.display(), "artifact missing");
                continue;
            }
            dependency_mounts.push(DependencyMount {
                host,
                relative: relative.clone(),
                exclude: dep_job.artifacts.exclude.clone(),
            });
        }
    }

    add_dependency_mounts(job, artifacts, &mut collector, dependency_mounts)?;

    let cache_specs = cache.mount_specs(
        &job.name,
        session_dir,
        &job.cache,
        workspace_root,
        cache_env,
    )?;
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

fn add_dependency_mounts(
    job: &JobSpec,
    artifacts: &ArtifactManager,
    collector: &mut MountCollector<'_>,
    mounts: Vec<DependencyMount>,
) -> Result<()> {
    let mut grouped: HashMap<PathBuf, Vec<DependencyMount>> = HashMap::new();
    for mount in mounts {
        grouped
            .entry(mount.relative.clone())
            .or_default()
            .push(mount);
    }

    for (relative, mounts) in grouped {
        let container = collector.container_path(&relative);
        let must_stage = mounts.len() > 1 || mounts.iter().any(|mount| !mount.exclude.is_empty());
        if !must_stage {
            collector.push(
                mounts.into_iter().next().expect("single mount").host,
                container,
                true,
            );
            continue;
        }

        let staged = stage_dependency_mount(artifacts, &job.name, &relative, &mounts)?;
        if staged.exists() {
            collector.push(staged, container, true);
        }
    }

    Ok(())
}

fn stage_dependency_mount(
    artifacts: &ArtifactManager,
    job_name: &str,
    relative: &Path,
    mounts: &[DependencyMount],
) -> Result<PathBuf> {
    let staged = artifacts.job_dependency_host_path(job_name, relative);
    if staged.exists() {
        remove_path(&staged)
            .with_context(|| format!("failed to clear staged dependency {}", staged.display()))?;
    }

    let any_dir = mounts.iter().any(|mount| mount.host.is_dir());
    if any_dir {
        fs::create_dir_all(&staged)
            .with_context(|| format!("failed to create {}", staged.display()))?;
        for mount in mounts {
            let exclude = build_exclude_matcher(&mount.exclude)?;
            if mount.host.is_dir() {
                copy_dir_contents(&mount.host, &staged, relative, exclude.as_ref())?;
            } else {
                copy_dependency_file(
                    &mount.host,
                    &staged.join(file_name_or_default(&mount.host)),
                    relative,
                    exclude.as_ref(),
                )?;
            }
        }
    } else {
        if let Some(parent) = staged.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        for mount in mounts {
            let exclude = build_exclude_matcher(&mount.exclude)?;
            copy_dependency_file(&mount.host, &staged, relative, exclude.as_ref())?;
        }
    }

    Ok(staged)
}

fn copy_dir_contents(
    src: &Path,
    dest: &Path,
    base_relative: &Path,
    exclude: Option<&GlobSet>,
) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let child_src = entry.path();
        let child_dest = dest.join(entry.file_name());
        let child_relative = base_relative.join(entry.file_name());
        copy_path(&child_src, &child_dest, &child_relative, exclude)?;
    }
    Ok(())
}

fn copy_dependency_file(
    src: &Path,
    dest: &Path,
    relative: &Path,
    exclude: Option<&GlobSet>,
) -> Result<()> {
    if should_exclude_artifact(relative, exclude) {
        return Ok(());
    }
    copy_path(src, dest, relative, exclude)
}

fn copy_path(src: &Path, dest: &Path, relative: &Path, exclude: Option<&GlobSet>) -> Result<()> {
    let metadata =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat {}", src.display()))?;
    if metadata.is_dir() {
        fs::create_dir_all(dest).with_context(|| format!("failed to create {}", dest.display()))?;
        copy_dir_contents(src, dest, relative, exclude)?;
        return Ok(());
    }
    if should_exclude_artifact(relative, exclude) {
        return Ok(());
    }

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(src, dest)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dest.display()))?;
    Ok(())
}

fn build_exclude_matcher(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            Glob::new(pattern)
                .with_context(|| format!("invalid artifacts.exclude pattern '{pattern}'"))?,
        );
    }
    Ok(Some(builder.build()?))
}

fn should_exclude_artifact(path: &Path, exclude: Option<&GlobSet>) -> bool {
    exclude.is_some_and(|glob| glob.is_match(path))
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
    }
}

fn file_name_or_default(path: &Path) -> OsString {
    path.file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| OsString::from("artifact"))
}

#[derive(Debug, Clone)]
struct DependencyMount {
    host: PathBuf,
    relative: PathBuf,
    exclude: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::{DependencyMount, MountCollector, add_dependency_mounts};
    use crate::model::{ArtifactSpec, JobSpec, RetryPolicySpec};
    use crate::pipeline::ArtifactManager;
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn add_dependency_mounts_merges_directory_artifacts_for_same_target() {
        let root = temp_path("dependency-merge");
        let artifacts = ArtifactManager::new(root.clone());
        let job = job("package-linux");
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&first).expect("create first");
        fs::create_dir_all(&second).expect("create second");
        fs::write(first.join("linux-debug.txt"), "debug").expect("write debug");
        fs::write(second.join("linux-release.txt"), "release").expect("write release");

        let mut collector = MountCollector::new(Path::new("/builds/opal"));
        add_dependency_mounts(
            &job,
            &artifacts,
            &mut collector,
            vec![
                DependencyMount {
                    host: first,
                    relative: PathBuf::from("tests-temp/build"),
                    exclude: Vec::new(),
                },
                DependencyMount {
                    host: second,
                    relative: PathBuf::from("tests-temp/build"),
                    exclude: Vec::new(),
                },
            ],
        )
        .expect("merge dependency mounts");

        let mounts = collector.into_mounts();
        assert_eq!(mounts.len(), 1);
        assert_eq!(
            mounts[0].container,
            PathBuf::from("/builds/opal/tests-temp/build")
        );
        assert!(mounts[0].host.join("linux-debug.txt").exists());
        assert!(mounts[0].host.join("linux-release.txt").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn add_dependency_mounts_applies_artifact_excludes_when_staging() {
        let root = temp_path("dependency-exclude");
        let artifacts = ArtifactManager::new(root.clone());
        let job = job("package-linux");
        let source = root.join("source");
        fs::create_dir_all(source.join("sub")).expect("create source dir");
        fs::write(source.join("include.txt"), "keep").expect("write include");
        fs::write(source.join("ignore.txt"), "skip").expect("write ignore");
        fs::write(source.join("sub").join("trace.log"), "skip").expect("write nested ignore");

        let mut collector = MountCollector::new(Path::new("/builds/opal"));
        add_dependency_mounts(
            &job,
            &artifacts,
            &mut collector,
            vec![DependencyMount {
                host: source,
                relative: PathBuf::from("tests-temp/filtered"),
                exclude: vec![
                    "tests-temp/filtered/ignore.txt".into(),
                    "tests-temp/filtered/**/*.log".into(),
                ],
            }],
        )
        .expect("stage filtered dependency mount");

        let mount = collector
            .into_mounts()
            .into_iter()
            .next()
            .expect("staged mount");
        assert!(mount.host.join("include.txt").exists());
        assert!(!mount.host.join("ignore.txt").exists());
        assert!(!mount.host.join("sub").join("trace.log").exists());

        let _ = fs::remove_dir_all(root);
    }

    fn job(name: &str) -> JobSpec {
        JobSpec {
            name: name.into(),
            stage: "test".into(),
            commands: vec!["echo ok".into()],
            needs: Vec::new(),
            explicit_needs: false,
            dependencies: Vec::new(),
            before_script: None,
            after_script: None,
            inherit_default_before_script: true,
            inherit_default_after_script: true,
            when: None,
            rules: Vec::new(),
            only: Vec::new(),
            except: Vec::new(),
            artifacts: ArtifactSpec::default(),
            cache: Vec::new(),
            image: None,
            variables: HashMap::new(),
            services: Vec::new(),
            timeout: None,
            retry: RetryPolicySpec::default(),
            interruptible: false,
            resource_group: None,
            parallel: None,
            tags: Vec::new(),
            environment: None,
        }
    }

    fn temp_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("opal-{prefix}-{nanos}"))
    }
}
