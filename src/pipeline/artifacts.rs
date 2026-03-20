use crate::git;
use crate::model::{ArtifactSourceOutcome, JobSpec};
use crate::naming::{job_name_slug, project_slug};
use anyhow::{Context, Result, anyhow};
use globset::{Glob, GlobSet, GlobSetBuilder};
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
        if job.artifacts.paths.is_empty() && !job.artifacts.untracked {
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
        if dep_job.artifacts.untracked {
            for relative in self.read_untracked_manifest(job_name) {
                let host = self.job_artifact_host_path(job_name, &relative);
                if !host.exists() {
                    continue;
                }
                if !dep_job.artifacts.when.includes(outcome) && !artifact_path_has_content(&host) {
                    continue;
                }
                specs.push((host, relative));
            }
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

    pub fn collect_untracked(&self, job: &JobSpec, workspace: &Path) -> Result<()> {
        if !job.artifacts.untracked {
            return Ok(());
        }

        let exclude = build_exclude_matcher(&job.artifacts.exclude)?;
        let explicit_paths = &job.artifacts.paths;
        let mut collected = Vec::new();
        for relative in git::untracked_files(workspace)? {
            let relative = PathBuf::from(relative);
            if path_is_covered_by_explicit_artifacts(&relative, explicit_paths) {
                continue;
            }
            if should_exclude(&relative, exclude.as_ref()) {
                continue;
            }

            let src = workspace.join(&relative);
            if !src.exists() {
                continue;
            }
            copy_untracked_entry(
                workspace,
                &src,
                self.job_artifact_host_path(&job.name, &relative),
                &relative,
                exclude.as_ref(),
                &mut collected,
            )?;
        }

        collected.sort();
        collected.dedup();
        self.write_untracked_manifest(&job.name, &collected)
    }

    fn write_untracked_manifest(&self, job_name: &str, paths: &[PathBuf]) -> Result<()> {
        let manifest = self.job_untracked_manifest_path(job_name);
        if let Some(parent) = manifest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let content = if paths.is_empty() {
            String::new()
        } else {
            let mut body = paths
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            body.push('\n');
            body
        };
        fs::write(&manifest, content)
            .with_context(|| format!("failed to write {}", manifest.display()))
    }

    fn read_untracked_manifest(&self, job_name: &str) -> Vec<PathBuf> {
        let manifest = self.job_untracked_manifest_path(job_name);
        let Ok(contents) = fs::read_to_string(&manifest) else {
            return Vec::new();
        };
        contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(PathBuf::from)
            .collect()
    }

    fn job_untracked_manifest_path(&self, job_name: &str) -> PathBuf {
        self.root
            .join(job_name_slug(job_name))
            .join("untracked-manifest.txt")
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

fn should_exclude(path: &Path, exclude: Option<&GlobSet>) -> bool {
    exclude.is_some_and(|glob| glob.is_match(path))
}

fn path_is_covered_by_explicit_artifacts(path: &Path, explicit_paths: &[PathBuf]) -> bool {
    explicit_paths
        .iter()
        .any(|artifact| match artifact_kind(artifact) {
            ArtifactPathKind::Directory => {
                let base = artifact_relative_path(artifact);
                path == base || path.starts_with(&base)
            }
            ArtifactPathKind::File => path == artifact_relative_path(artifact),
        })
}

fn copy_untracked_entry(
    workspace: &Path,
    src: &Path,
    dest: PathBuf,
    relative: &Path,
    exclude: Option<&GlobSet>,
    collected: &mut Vec<PathBuf>,
) -> Result<()> {
    let metadata =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat {}", src.display()))?;
    if metadata.is_dir() {
        for entry in
            fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))?
        {
            let entry = entry?;
            let child_src = entry.path();
            let child_relative = match child_src.strip_prefix(workspace) {
                Ok(rel) => rel.to_path_buf(),
                Err(_) => continue,
            };
            let child_dest = dest.join(entry.file_name());
            copy_untracked_entry(
                workspace,
                &child_src,
                child_dest,
                &child_relative,
                exclude,
                collected,
            )?;
        }
        return Ok(());
    }

    if should_exclude(relative, exclude) {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(src, &dest)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dest.display()))?;
    collected.push(relative.to_path_buf());
    Ok(())
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
    use super::{
        ArtifactManager, ArtifactPathKind, artifact_kind, artifact_path_has_content,
        path_is_covered_by_explicit_artifacts,
    };
    use crate::model::{
        ArtifactSourceOutcome, ArtifactSpec, ArtifactWhenSpec, JobSpec, RetryPolicySpec,
    };
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
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
        let job = job(
            "build",
            vec![relative.clone()],
            Vec::new(),
            false,
            ArtifactWhenSpec::OnSuccess,
        );
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

    #[test]
    fn collect_untracked_includes_ignored_workspace_files() {
        let root = temp_path("artifact-untracked");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("create workspace");
        let repo = git2::Repository::init(&workspace).expect("init repo");
        fs::write(workspace.join("README.md"), "opal\n").expect("write readme");
        fs::write(workspace.join(".gitignore"), "tests-temp/\n").expect("write ignore");
        let mut index = repo.index().expect("open index");
        index
            .add_path(Path::new("README.md"))
            .expect("add readme to index");
        index
            .add_path(Path::new(".gitignore"))
            .expect("add ignore to index");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = git2::Signature::now("Opal Tests", "opal@example.com").expect("signature");
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .expect("commit");

        fs::create_dir_all(workspace.join("tests-temp")).expect("create ignored dir");
        fs::write(workspace.join("tests-temp/generated.txt"), "hello").expect("write ignored");
        fs::write(workspace.join("scratch.txt"), "hi").expect("write untracked");

        let manager = ArtifactManager::new(root.join("artifacts"));
        let job = job(
            "build",
            Vec::new(),
            vec!["tests-temp/**/*.log".into()],
            true,
            ArtifactWhenSpec::OnSuccess,
        );

        manager
            .prepare_targets(&job)
            .expect("prepare artifact targets");
        manager
            .collect_untracked(&job, &workspace)
            .expect("collect untracked artifacts");

        let manifest = manager.read_untracked_manifest("build");
        assert!(manifest.iter().any(|path| path == Path::new("scratch.txt")));
        assert!(
            manifest
                .iter()
                .any(|path| path == Path::new("tests-temp/generated.txt"))
        );
        assert!(
            manager
                .job_artifact_host_path("build", Path::new("scratch.txt"))
                .exists()
        );
        assert!(
            manager
                .job_artifact_host_path("build", Path::new("tests-temp/generated.txt"))
                .exists()
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn path_is_covered_by_explicit_artifacts_matches_directory_and_file_paths() {
        assert!(path_is_covered_by_explicit_artifacts(
            Path::new("tests-temp/build/linux.txt"),
            &[PathBuf::from("tests-temp/build/")]
        ));
        assert!(path_is_covered_by_explicit_artifacts(
            Path::new("output/report.txt"),
            &[PathBuf::from("output/report.txt")]
        ));
        assert!(!path_is_covered_by_explicit_artifacts(
            Path::new("other.txt"),
            &[PathBuf::from("output/report.txt")]
        ));
    }

    fn job(
        name: &str,
        paths: Vec<PathBuf>,
        exclude: Vec<String>,
        untracked: bool,
        when: ArtifactWhenSpec,
    ) -> JobSpec {
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
            artifacts: ArtifactSpec {
                paths,
                exclude,
                untracked,
                when,
            },
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
