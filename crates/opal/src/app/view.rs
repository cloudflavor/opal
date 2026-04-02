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
    Ok(load_history()
        .await?
        .into_iter()
        .filter(|entry| entry.scope_root.as_deref() == Some(scope_root.as_str()))
        .collect())
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
