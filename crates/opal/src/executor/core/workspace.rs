use super::ExecutorCore;
use crate::model::JobSpec;
use crate::naming::job_name_slug;
use crate::pipeline::VolumeMount;
use anyhow::{Context, Result};
use git2::{IndexAddOption, Repository, Signature, StatusOptions};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) struct PreparedWorkspace {
    pub host_workdir: PathBuf,
    pub mounts: Vec<VolumeMount>,
}

pub(super) fn prepare_job_workspace(
    exec: &ExecutorCore,
    job: &JobSpec,
) -> Result<PreparedWorkspace> {
    let host_workdir = exec
        .session_dir
        .join("workspaces")
        .join(job_name_slug(&job.name));
    if host_workdir.exists() {
        fs::remove_dir_all(&host_workdir)
            .with_context(|| format!("failed to clear {}", host_workdir.display()))?;
    }
    fs::create_dir_all(&host_workdir)
        .with_context(|| format!("failed to create {}", host_workdir.display()))?;

    copy_workspace_contents(&exec.config.workdir, &host_workdir)?;
    refresh_git_snapshot_state(&host_workdir)?;

    Ok(PreparedWorkspace {
        host_workdir,
        mounts: Vec::new(),
    })
}

fn refresh_git_snapshot_state(workdir: &Path) -> Result<()> {
    let repo = match Repository::open(workdir) {
        Ok(repo) => repo,
        Err(_) => return Ok(()),
    };

    let mut status_options = StatusOptions::new();
    status_options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);
    let statuses = repo
        .statuses(Some(&mut status_options))
        .context("failed to inspect git status for workspace snapshot")?;
    if statuses.is_empty() {
        return Ok(());
    }

    let mut index = repo
        .index()
        .context("failed to open git index for workspace snapshot")?;
    index
        .add_all(["*"], IndexAddOption::DEFAULT, None)
        .context("failed to refresh git index for workspace snapshot")?;
    index
        .write()
        .context("failed to write refreshed git index for workspace snapshot")?;

    let tree_id = index
        .write_tree()
        .context("failed to write tree for workspace snapshot")?;
    let tree = repo
        .find_tree(tree_id)
        .context("failed to find written tree for workspace snapshot")?;
    let signature = Signature::now("Opal", "opal@local.invalid")
        .context("failed to create snapshot git signature")?;

    if let Ok(parent) = repo.head().and_then(|head| head.peel_to_commit()) {
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "opal workspace snapshot",
            &tree,
            &[&parent],
        )
        .context("failed to commit workspace snapshot state")?;
    } else {
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "opal workspace snapshot",
            &tree,
            &[],
        )
        .context("failed to create initial workspace snapshot commit")?;
    }

    Ok(())
}

fn copy_workspace_contents(src: &Path, dest: &Path) -> Result<()> {
    let repo = Repository::discover(src).ok();
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let file_name = entry.file_name();
        let rel = PathBuf::from(&file_name);
        if should_exclude(src, &rel, repo.as_ref()) {
            continue;
        }

        let src_path = entry.path();
        let dest_path = dest.join(&file_name);
        copy_entry(src, repo.as_ref(), &rel, &src_path, &dest_path)?;
    }
    Ok(())
}

fn should_exclude(workspace_root: &Path, rel: &Path, repo: Option<&Repository>) -> bool {
    let hardcoded = matches!(
        rel.file_name().and_then(|name| name.to_str()),
        Some(
            ".opal"
                | "target"
                | "tests-temp"
                | "node_modules"
                | ".svelte-kit"
                | ".wrangler"
                | ".output"
                | ".vercel"
                | ".netlify"
                | "build"
        )
    );
    if hardcoded {
        return true;
    }
    if rel.starts_with(".git") {
        return false;
    }
    let Some(repo) = repo else {
        return false;
    };
    let candidate = workspace_root.join(rel);
    let Ok(path) = candidate.strip_prefix(workspace_root) else {
        return false;
    };
    is_git_ignored(workspace_root, path)
        .unwrap_or_else(|| repo.status_should_ignore(path).unwrap_or(false))
}

fn is_git_ignored(workspace_root: &Path, rel: &Path) -> Option<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("check-ignore")
        .arg("-q")
        .arg(rel)
        .status()
        .ok()?;
    Some(status.success())
}

fn copy_entry(
    workspace_root: &Path,
    repo: Option<&Repository>,
    rel: &Path,
    src: &Path,
    dest: &Path,
) -> Result<()> {
    let metadata =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat {}", src.display()))?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        copy_symlink(src, dest)
    } else if file_type.is_dir() {
        fs::create_dir_all(dest).with_context(|| format!("failed to create {}", dest.display()))?;
        for entry in
            fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))?
        {
            let entry = entry?;
            let child_name = entry.file_name();
            let child_rel = rel.join(&child_name);
            if should_exclude(workspace_root, &child_rel, repo) {
                continue;
            }
            let child_src = entry.path();
            let child_dest = dest.join(child_name);
            copy_entry(workspace_root, repo, &child_rel, &child_src, &child_dest)?;
        }
        Ok(())
    } else {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::copy(src, dest)
            .with_context(|| format!("failed to copy {} to {}", src.display(), dest.display()))?;
        fs::set_permissions(dest, metadata.permissions())
            .with_context(|| format!("failed to set permissions on {}", dest.display()))?;
        Ok(())
    }
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dest: &Path) -> Result<()> {
    let target =
        fs::read_link(src).with_context(|| format!("failed to read link {}", src.display()))?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    unix_fs::symlink(&target, dest)
        .with_context(|| format!("failed to recreate symlink {}", dest.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(src: &Path, dest: &Path) -> Result<()> {
    let target =
        fs::read_link(src).with_context(|| format!("failed to read link {}", src.display()))?;
    if target.is_dir() {
        fs::create_dir_all(dest).with_context(|| format!("failed to create {}", dest.display()))?;
        Ok(())
    } else {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::copy(src, dest)
            .with_context(|| format!("failed to copy {} to {}", src.display(), dest.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{copy_workspace_contents, prepare_job_workspace};
    use crate::config::OpalConfig;
    use crate::executor::core::{
        ExecutorCore, history_store::HistoryStore, runtime_state::RuntimeState,
        stage_tracker::StageTracker,
    };
    use crate::model::{
        ArtifactSpec, JobSpec, PipelineDefaultsSpec, PipelineSpec, RetryPolicySpec, StageSpec,
    };
    use crate::{EngineKind, ExecutorConfig};
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn copy_workspace_contents_excludes_runtime_heavy_dirs() {
        let src = temp_path("workspace-src");
        let dest = temp_path("workspace-dest");
        git2::Repository::init(&src).expect("init repo");
        fs::create_dir_all(src.join("src")).expect("create src dir");
        fs::create_dir_all(src.join("target")).expect("create target dir");
        fs::create_dir_all(src.join("tests-temp")).expect("create tests-temp dir");
        fs::create_dir_all(src.join(".opal")).expect("create .opal dir");
        fs::create_dir_all(src.join("ignored-dir")).expect("create ignored dir");
        fs::write(src.join("Cargo.toml"), "[package]").expect("write cargo");
        fs::write(src.join(".gitignore"), "ignored-dir/\nignored.txt\n").expect("write gitignore");
        fs::write(src.join("src").join("main.rs"), "fn main() {}").expect("write source");
        fs::write(src.join("target").join("keep.out"), "nope").expect("write target");
        fs::write(src.join("ignored-dir").join("foo.txt"), "nope").expect("write ignored dir file");
        fs::write(src.join("ignored.txt"), "nope").expect("write ignored file");

        fs::create_dir_all(&dest).expect("create dest");
        copy_workspace_contents(&src, &dest).expect("copy workspace");

        assert!(dest.join("Cargo.toml").exists());
        assert!(dest.join("src").join("main.rs").exists());
        assert!(!dest.join("target").exists());
        assert!(!dest.join("tests-temp").exists());
        assert!(!dest.join(".opal").exists());
        assert!(!dest.join("ignored-dir").exists());
        assert!(!dest.join("ignored.txt").exists());

        let _ = fs::remove_dir_all(src);
        let _ = fs::remove_dir_all(dest);
    }

    #[test]
    fn copy_workspace_contents_respects_nested_gitignore_entries() {
        let src = temp_path("workspace-src-nested-ignore");
        let dest = temp_path("workspace-dest-nested-ignore");
        git2::Repository::init(&src).expect("init repo");
        fs::create_dir_all(src.join("ui-docs").join("node_modules").join("pkg"))
            .expect("create nested ignored dir");
        fs::write(src.join("ui-docs").join(".gitignore"), "node_modules/\n")
            .expect("write nested gitignore");
        fs::write(src.join("ui-docs").join("package.json"), "{}").expect("write package file");
        fs::write(
            src.join("ui-docs")
                .join("node_modules")
                .join("pkg")
                .join("index.js"),
            "console.log('ignore')",
        )
        .expect("write ignored nested file");

        fs::create_dir_all(&dest).expect("create dest");
        copy_workspace_contents(&src, &dest).expect("copy workspace");

        assert!(dest.join("ui-docs").join("package.json").exists());
        assert!(!dest.join("ui-docs").join("node_modules").exists());

        let _ = fs::remove_dir_all(src);
        let _ = fs::remove_dir_all(dest);
    }

    #[test]
    fn copy_workspace_contents_excludes_nested_runtime_heavy_dirs() {
        let src = temp_path("workspace-src-nested-heavy");
        let dest = temp_path("workspace-dest-nested-heavy");
        git2::Repository::init(&src).expect("init repo");
        fs::create_dir_all(src.join("ui-docs").join("node_modules").join("pkg"))
            .expect("create nested node_modules");
        fs::create_dir_all(src.join("ui-docs").join(".svelte-kit").join("generated"))
            .expect("create nested svelte kit");
        fs::write(src.join("ui-docs").join("package.json"), "{}").expect("write package file");
        fs::write(
            src.join("ui-docs")
                .join("node_modules")
                .join("pkg")
                .join("index.js"),
            "console.log('ignore')",
        )
        .expect("write nested module file");
        fs::write(
            src.join("ui-docs")
                .join(".svelte-kit")
                .join("generated")
                .join("root.js"),
            "export {}",
        )
        .expect("write generated file");

        fs::create_dir_all(&dest).expect("create dest");
        copy_workspace_contents(&src, &dest).expect("copy workspace");

        assert!(dest.join("ui-docs").join("package.json").exists());
        assert!(!dest.join("ui-docs").join("node_modules").exists());
        assert!(!dest.join("ui-docs").join(".svelte-kit").exists());

        let _ = fs::remove_dir_all(src);
        let _ = fs::remove_dir_all(dest);
    }

    #[test]
    fn prepare_job_workspace_copies_git_dir() {
        let workdir = temp_path("workspace-host");
        fs::create_dir_all(workdir.join(".git")).expect("create git dir");
        fs::write(workdir.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("write head");
        fs::write(workdir.join("Cargo.toml"), "[package]").expect("write cargo");

        let session_dir = temp_path("workspace-session");
        fs::create_dir_all(&session_dir).expect("create session");
        let exec = test_core(workdir.clone(), session_dir.clone());
        let prepared = prepare_job_workspace(&exec, &job("build")).expect("prepare workspace");

        assert!(prepared.host_workdir.join("Cargo.toml").exists());
        assert!(prepared.host_workdir.join(".git").join("HEAD").exists());
        assert!(prepared.mounts.is_empty());

        let _ = fs::remove_dir_all(workdir);
        let _ = fs::remove_dir_all(session_dir);
    }

    fn test_core(workdir: PathBuf, session_dir: PathBuf) -> ExecutorCore {
        ExecutorCore {
            config: ExecutorConfig {
                pipeline: workdir.join(".gitlab-ci.yml"),
                workdir: workdir.clone(),
                image: None,
                env_includes: Vec::new(),
                selected_jobs: Vec::new(),
                max_parallel_jobs: 1,
                enable_tui: false,
                emit_console_output: false,
                engine: EngineKind::Docker,
                gitlab: None,
                settings: OpalConfig::default(),
                trace_scripts: false,
            },
            pipeline: PipelineSpec {
                stages: vec![StageSpec {
                    name: "test".into(),
                    jobs: vec!["build".into()],
                }],
                jobs: HashMap::new(),
                defaults: PipelineDefaultsSpec {
                    image: Some("docker.io/library/alpine:3.19".into()),
                    before_script: Vec::new(),
                    after_script: Vec::new(),
                    variables: HashMap::new(),
                    cache: Vec::new(),
                    services: Vec::new(),
                    timeout: None,
                    retry: RetryPolicySpec::default(),
                    interruptible: false,
                },
                workflow: None,
                filters: Default::default(),
            },
            use_color: false,
            scripts_dir: session_dir.join("scripts"),
            logs_dir: session_dir.join("logs"),
            session_dir,
            container_session_dir: PathBuf::from("/opal/run"),
            run_id: "run".into(),
            verbose_scripts: false,
            env_vars: Vec::new(),
            shared_env: HashMap::new(),
            container_workdir: Path::new("/builds").join("workspace-host"),
            stage_tracker: StageTracker::new(&[]),
            runtime_state: RuntimeState::default(),
            history_store: HistoryStore::load(PathBuf::from("/tmp/opal-workspace-history.json")),
            secrets: Default::default(),
            artifacts: crate::pipeline::ArtifactManager::new(PathBuf::from("/tmp/artifacts")),
            cache: crate::pipeline::CacheManager::new(PathBuf::from("/tmp/cache")),
            external_artifacts: None,
            bootstrap_mounts: Vec::new(),
        }
    }

    fn job(name: &str) -> JobSpec {
        JobSpec {
            name: name.into(),
            stage: "test".into(),
            commands: vec!["echo hi".into()],
            needs: Vec::new(),
            explicit_needs: false,
            dependencies: Vec::new(),
            before_script: None,
            after_script: None,
            inherit_default_before_script: true,
            inherit_default_after_script: true,
            inherit_default_image: true,
            inherit_default_cache: true,
            inherit_default_services: true,
            inherit_default_timeout: true,
            inherit_default_retry: true,
            inherit_default_interruptible: true,
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
