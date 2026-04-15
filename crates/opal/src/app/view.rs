use super::OpalApp;
use super::context::history_scope_root;
use crate::ViewArgs;
use crate::history::{self, HistoryEntry, HistoryJob};
use crate::runtime;
use crate::ui;
use anyhow::{Context, Result};
use std::path::Path;

pub(crate) async fn execute(app: &OpalApp, args: ViewArgs) -> Result<()> {
    let workdir = app.resolve_workdir(args.workdir);
    ui::view_pipeline_logs(&workdir).await
}

pub(crate) async fn load_history() -> Result<Vec<HistoryEntry>> {
    history::load_async(&runtime::history_path()).await
}

pub(crate) async fn load_history_for_workdir(workdir: &Path) -> Result<Vec<HistoryEntry>> {
    let scope_root = history_scope_root(workdir);
    let mut scoped = Vec::new();
    let mut legacy = Vec::new();
    for entry in load_history().await? {
        match entry.scope_root.as_deref() {
            Some(root) if root == scope_root.as_str() => scoped.push(entry),
            None => legacy.push(entry),
            _ => {}
        }
    }
    if scoped.is_empty() {
        Ok(legacy)
    } else {
        Ok(scoped)
    }
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

#[cfg(test)]
mod tests {
    use super::load_history_for_workdir;
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
}
