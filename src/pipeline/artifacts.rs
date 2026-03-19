use crate::model::{ArtifactSourceOutcome, JobSpec};
use crate::naming::{job_name_slug, project_slug};
use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tracing::warn;

#[derive(Debug, Clone)]
pub struct ArtifactManager {
    root: PathBuf,
}

impl ArtifactManager {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn prepare_targets(&self, job: &JobSpec) -> Result<()> {
        if job.artifacts.paths.is_empty() {
            return Ok(());
        }
        let root = self.job_artifacts_root(&job.name);
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to prepare artifacts for {}", job.name))?;

        for relative in &job.artifacts.paths {
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
                }
            }
        }

        Ok(())
    }

    pub fn job_mount_specs(&self, job: &JobSpec) -> Vec<(PathBuf, PathBuf)> {
        use std::collections::HashSet;

        let mut specs = Vec::new();
        let mut seen = HashSet::new();
        for relative in &job.artifacts.paths {
            let rel_path = artifact_relative_path(relative);
            let mount_rel = match artifact_kind(relative) {
                ArtifactPathKind::Directory => rel_path.clone(),
                ArtifactPathKind::File => match rel_path.parent() {
                    Some(parent) if parent != Path::new("") && parent != Path::new(".") => {
                        parent.to_path_buf()
                    }
                    _ => continue,
                },
            };
            if seen.insert(mount_rel.clone()) {
                let host = self.job_artifacts_root(&job.name).join(&mount_rel);
                specs.push((host, mount_rel));
            }
        }
        specs
    }

    pub fn dependency_mount_specs(
        &self,
        job_name: &str,
        job: Option<&JobSpec>,
        outcome: Option<ArtifactSourceOutcome>,
        optional: bool,
    ) -> Vec<(PathBuf, PathBuf)> {
        let Some(dep_job) = job else {
            return Vec::new();
        };
        let mut specs = Vec::new();
        for relative in &dep_job.artifacts.paths {
            let host = self.job_artifact_host_path(job_name, relative);
            if !host.exists() {
                if !optional {
                    warn!(job = job_name, path = %relative.display(), "artifact missing");
                }
                continue;
            }
            if !dep_job.artifacts.when.includes(outcome) && !artifact_path_has_content(&host) {
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

    pub fn job_artifacts_root(&self, job_name: &str) -> PathBuf {
        self.root.join(job_name_slug(job_name)).join("artifacts")
    }

    pub fn job_dependency_root(&self, job_name: &str) -> PathBuf {
        self.root.join(job_name_slug(job_name)).join("dependencies")
    }

    pub fn job_dependency_host_path(&self, job_name: &str, artifact: &Path) -> PathBuf {
        self.job_dependency_root(job_name)
            .join(artifact_relative_path(artifact))
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
    let text = path.to_string_lossy();
    if text.ends_with(std::path::MAIN_SEPARATOR) {
        ArtifactPathKind::Directory
    } else {
        ArtifactPathKind::File
    }
}

fn artifact_path_has_content(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => true,
        Ok(metadata) if metadata.is_dir() => fs::read_dir(path)
            .ok()
            .and_then(|mut entries| entries.next())
            .is_some(),
        _ => false,
    }
}

#[derive(Clone, Debug)]
pub struct ExternalArtifactsManager {
    inner: Arc<ExternalArtifactsInner>,
}

#[derive(Debug)]
struct ExternalArtifactsInner {
    root: PathBuf,
    base_url: String,
    token: String,
    cache: Mutex<HashMap<String, PathBuf>>,
}

impl ExternalArtifactsManager {
    pub fn new(root: PathBuf, base_url: String, token: String) -> Self {
        let inner = ExternalArtifactsInner {
            root,
            base_url,
            token,
            cache: Mutex::new(HashMap::new()),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    pub fn ensure_artifacts(&self, project: &str, job: &str, reference: &str) -> Result<PathBuf> {
        let key = format!("{project}::{reference}::{job}");
        if let Ok(cache) = self.inner.cache.lock()
            && let Some(path) = cache.get(&key)
            && path.exists()
        {
            return Ok(path.clone());
        }

        let target = self.external_root(project, job, reference);
        if target.exists() {
            fs::remove_dir_all(&target)
                .with_context(|| format!("failed to clear {}", target.display()))?;
        }
        fs::create_dir_all(&target)
            .with_context(|| format!("failed to create {}", target.display()))?;
        let archive_path = target.join("artifacts.zip");
        self.download_artifacts(project, job, reference, &archive_path)?;
        self.extract_artifacts(&archive_path, &target)?;
        let _ = fs::remove_file(&archive_path);

        if let Ok(mut cache) = self.inner.cache.lock() {
            cache.insert(key, target.clone());
        }

        Ok(target)
    }

    fn external_root(&self, project: &str, job: &str, reference: &str) -> PathBuf {
        let project_slug = project_slug(project);
        let reference_slug = sanitize_reference(reference);
        self.inner
            .root
            .join("external")
            .join(project_slug)
            .join(reference_slug)
            .join(job_name_slug(job))
    }

    fn download_artifacts(
        &self,
        project: &str,
        job: &str,
        reference: &str,
        dest: &Path,
    ) -> Result<()> {
        let base = self.inner.base_url.trim_end_matches('/');
        let project_id = percent_encode(project);
        let ref_id = percent_encode(reference);
        let job_name = percent_encode(job);
        let url = format!(
            "{base}/api/v4/projects/{project_id}/jobs/artifacts/{ref_id}/download?job={job_name}"
        );
        let status = Command::new("curl")
            .arg("--fail")
            .arg("-sS")
            .arg("-L")
            .arg("-H")
            .arg(format!("PRIVATE-TOKEN: {}", self.inner.token))
            .arg("-o")
            .arg(dest)
            .arg(&url)
            .status()
            .with_context(|| "failed to invoke curl to download artifacts")?;
        if !status.success() {
            return Err(anyhow!(
                "curl failed to download artifacts from {} (status {})",
                url,
                status.code().unwrap_or(-1)
            ));
        }
        Ok(())
    }

    fn extract_artifacts(&self, archive: &Path, dest: &Path) -> Result<()> {
        let unzip_status = Command::new("unzip")
            .arg("-q")
            .arg("-o")
            .arg(archive)
            .arg("-d")
            .arg(dest)
            .status();
        match unzip_status {
            Ok(status) if status.success() => return Ok(()),
            Ok(_) | Err(_) => {
                // fallback to python's zipfile
                let script =
                    "import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])";
                let status = Command::new("python3")
                    .arg("-c")
                    .arg(script)
                    .arg(archive)
                    .arg(dest)
                    .status()
                    .with_context(|| "failed to invoke python3 to extract artifacts")?;
                if status.success() {
                    return Ok(());
                }
            }
        }
        Err(anyhow!(
            "unable to extract artifacts archive {}",
            archive.display()
        ))
    }
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

fn sanitize_reference(reference: &str) -> String {
    let mut slug = String::new();
    for ch in reference.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | '.') {
            slug.push(ch);
        } else {
            slug.push('-');
        }
    }
    if slug.is_empty() {
        slug.push_str("ref");
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::{ArtifactManager, ArtifactPathKind, artifact_kind, artifact_path_has_content};
    use crate::model::{
        ArtifactSourceOutcome, ArtifactSpec, ArtifactWhenSpec, JobSpec, RetryPolicySpec,
    };
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn artifact_path_kind_treats_trailing_separator_as_directory() {
        assert!(matches!(
            artifact_kind(std::path::Path::new("tests-temp/build/")),
            ArtifactPathKind::Directory
        ));
    }

    #[test]
    fn artifact_path_has_content_requires_directory_entries() {
        let root = temp_path("artifact-content");
        fs::create_dir_all(&root).expect("create dir");
        assert!(!artifact_path_has_content(&root));
        fs::write(root.join("marker.txt"), "ok").expect("write marker");
        assert!(artifact_path_has_content(&root));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dependency_mount_specs_allow_populated_artifacts_without_recorded_outcome() {
        let root = temp_path("artifact-presence");
        let manager = ArtifactManager::new(root.clone());
        let relative = PathBuf::from("tests-temp/build/");
        let job = job("build", vec![relative.clone()], ArtifactWhenSpec::OnSuccess);
        let host = manager.job_artifact_host_path("build", &relative);
        fs::create_dir_all(&host).expect("create artifact dir");
        fs::write(host.join("linux-release.txt"), "release").expect("write artifact");

        let specs = manager.dependency_mount_specs("build", Some(&job), None, false);

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].0, host);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn artifact_when_matches_expected_outcomes() {
        assert!(ArtifactWhenSpec::Always.includes(None));
        assert!(ArtifactWhenSpec::Always.includes(Some(ArtifactSourceOutcome::Success)));
        assert!(ArtifactWhenSpec::OnSuccess.includes(Some(ArtifactSourceOutcome::Success)));
        assert!(!ArtifactWhenSpec::OnSuccess.includes(Some(ArtifactSourceOutcome::Failed)));
        assert!(ArtifactWhenSpec::OnFailure.includes(Some(ArtifactSourceOutcome::Failed)));
        assert!(!ArtifactWhenSpec::OnFailure.includes(Some(ArtifactSourceOutcome::Skipped)));
        assert!(!ArtifactWhenSpec::OnFailure.includes(None));
    }

    fn job(name: &str, paths: Vec<PathBuf>, when: ArtifactWhenSpec) -> JobSpec {
        JobSpec {
            name: name.into(),
            stage: "build".into(),
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
            artifacts: ArtifactSpec { paths, when },
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
