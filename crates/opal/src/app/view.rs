use super::OpalApp;
use super::context::history_scope_root;
use crate::ViewArgs;
use crate::history::{self, HistoryEntry, HistoryJob};
use crate::runtime;
use crate::ui;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub(crate) async fn execute(app: &OpalApp, args: ViewArgs) -> Result<()> {
    let workdir = app.resolve_workdir(args.workdir);
    ui::view_pipeline_logs(&workdir).await
}

pub(crate) async fn load_history() -> Result<Vec<HistoryEntry>> {
    history::load_async(&runtime::history_path()).await
}

pub(crate) async fn load_history_for_workdir(workdir: &Path) -> Result<Vec<HistoryEntry>> {
    let current_repo_key = repository_identity_key(workdir);
    let scope_root = history_scope_root(workdir);
    Ok(select_history_for_scope(
        load_history().await?,
        &scope_root,
        current_repo_key.as_deref(),
        |root| repository_identity_key(Path::new(root)),
    ))
}

pub(crate) async fn latest_history_entry_for_workdir(
    workdir: &Path,
) -> Result<Option<HistoryEntry>> {
    Ok(load_history_for_workdir(workdir).await?.into_iter().last())
}

pub(crate) async fn find_history_entry_for_workdir(
    workdir: &Path,
    run_id: &str,
) -> Result<Option<HistoryEntry>> {
    Ok(load_history_for_workdir(workdir)
        .await?
        .into_iter()
        .find(|entry| entry.run_id == run_id))
}

pub(crate) fn find_job<'a>(entry: &'a HistoryEntry, name: &str) -> Option<&'a HistoryJob> {
    entry.jobs.iter().find(|job| job.name == name)
}

pub(crate) async fn read_job_log(entry: &HistoryEntry, job: &HistoryJob) -> Result<String> {
    let path = job
        .log_path
        .as_deref()
        .map(Path::new)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| runtime::logs_dir(&entry.run_id).join(format!("{}.log", job.log_hash)));
    tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("failed to read job log {}", path.display()))
}

pub(crate) async fn read_runtime_summary(job: &HistoryJob) -> Result<Option<String>> {
    let Some(path) = job.runtime_summary_path.as_deref() else {
        return Ok(None);
    };
    Ok(Some(tokio::fs::read_to_string(path).await.with_context(
        || format!("failed to read runtime summary {path}"),
    )?))
}

fn repository_identity_key(workdir: &Path) -> Option<String> {
    let common_dir = crate::git::repository_common_dir(workdir).ok()?;
    let canonical = fs::canonicalize(&common_dir).unwrap_or(common_dir);
    Some(canonical.display().to_string())
}

fn select_history_for_scope<F>(
    entries: Vec<HistoryEntry>,
    scope_root: &str,
    current_repo_key: Option<&str>,
    mut resolve_repo_key: F,
) -> Vec<HistoryEntry>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut scoped = Vec::new();
    let mut legacy = Vec::new();
    let mut other_scoped = Vec::new();
    for entry in entries {
        match entry.scope_root.as_deref() {
            Some(root) if root == scope_root => scoped.push(entry),
            None => legacy.push(entry),
            Some(_) => other_scoped.push(entry),
        }
    }
    if !scoped.is_empty() {
        return scoped;
    }
    if !legacy.is_empty() {
        return legacy;
    }
    let Some(current_repo_key) = current_repo_key else {
        return Vec::new();
    };
    let mut cache = HashMap::<String, Option<String>>::new();
    other_scoped
        .into_iter()
        .filter(|entry| {
            let Some(root) = entry.scope_root.as_deref() else {
                return false;
            };
            let key = cache
                .entry(root.to_string())
                .or_insert_with(|| resolve_repo_key(root));
            key.as_deref() == Some(current_repo_key)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{load_history_for_workdir, select_history_for_scope};
    use crate::app::context::history_scope_root;
    use crate::history::{HistoryEntry, HistoryStatus, save};
    use crate::mcp::TEST_ENV_LOCK;
    use crate::runtime;
    use std::env;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn load_history_for_workdir_includes_legacy_entries_when_scope_is_missing() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-view-legacy");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                history_entry("run-legacy-1", None),
                history_entry("run-legacy-2", None),
            ],
        )
        .expect("save history");

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let loaded = runtime
            .block_on(load_history_for_workdir(Path::new(".")))
            .expect("load history");

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].run_id, "run-legacy-1");
        assert_eq!(loaded[1].run_id, "run-legacy-2");
        unsafe {
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn load_history_for_workdir_prefers_scoped_entries_over_legacy_fallback() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-view-scope");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        let expected_scope = history_scope_root(Path::new("."));
        save(
            &runtime::history_path(),
            &[
                history_entry("run-legacy", None),
                history_entry("run-local", Some(expected_scope.as_str())),
                history_entry("run-foreign", Some("/tmp/other-repo")),
            ],
        )
        .expect("save history");

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let loaded = runtime
            .block_on(load_history_for_workdir(Path::new(".")))
            .expect("load history");

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].run_id, "run-local");
        unsafe {
            env::remove_var("OPAL_HOME");
        }
    }

    fn history_entry(run_id: &str, scope_root: Option<&str>) -> HistoryEntry {
        HistoryEntry {
            run_id: run_id.to_string(),
            finished_at: "2026-03-31T12:00:00Z".to_string(),
            status: HistoryStatus::Success,
            scope_root: scope_root.map(ToOwned::to_owned),
            ref_name: None,
            pipeline_file: None,
            jobs: Vec::new(),
        }
    }

    #[test]
    fn select_history_for_scope_accepts_same_repo_worktree_entries_when_exact_scope_is_missing() {
        let loaded = select_history_for_scope(
            vec![
                history_entry("run-worktree-1", Some("/tmp/repo-worktree-1")),
                history_entry("run-worktree-2", Some("/tmp/repo-worktree-2")),
                history_entry("run-foreign", Some("/tmp/other-repo")),
            ],
            "/tmp/repo-main",
            Some("repo-key-main"),
            |root| match root {
                "/tmp/repo-worktree-1" | "/tmp/repo-worktree-2" => Some("repo-key-main".into()),
                _ => Some("repo-key-foreign".into()),
            },
        );

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].run_id, "run-worktree-1");
        assert_eq!(loaded[1].run_id, "run-worktree-2");
    }
}
