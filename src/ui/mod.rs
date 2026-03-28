mod handle;
mod runner;
mod state;
pub mod types;

use crate::history;
use crate::history::HistoryEntry;
use crate::runtime;
use crate::ui::types::UiEvent;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::env;
use std::path::Path;
use tokio::sync::mpsc;

pub use handle::{UiBridge, UiHandle};
pub use types::{UiCommand, UiJobInfo, UiJobResources, UiJobStatus};

// TODO: DO NOT ADD CODE in mod.rs
pub fn view_history(history: Vec<HistoryEntry>, current_run_id: String) -> Result<()> {
    let (tx, rx) = mpsc::unbounded_channel();
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
    let workdir = env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let pipeline_path = workdir.join(".gitlab-ci.yml");
    let runner = runner::UiRunner::new(
        Vec::new(),
        history,
        current_run_id,
        HashMap::new(),
        String::new(),
        workdir,
        pipeline_path,
        tx.clone(),
        rx,
        cmd_tx,
    )?;
    let _ = tx.send(UiEvent::PipelineFinished);
    runner.run()
}

pub fn view_pipeline_logs(_root: &Path) -> Result<()> {
    let history_path = runtime::history_path();
    let history = history::load(&history_path)
        .with_context(|| format!("failed to load history at {}", history_path.display()))?;
    if history.is_empty() {
        anyhow::bail!("no history entries found at {}", history_path.display());
    }
    let current_run_id = history
        .last()
        .map(|entry| entry.run_id.clone())
        .unwrap_or_else(|| "history-view".to_string());
    let (tx, rx) = mpsc::unbounded_channel();
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
    let workdir = env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let pipeline_path = workdir.join(".gitlab-ci.yml");
    let runner = runner::UiRunner::new(
        Vec::new(),
        history,
        current_run_id,
        HashMap::new(),
        String::new(),
        workdir,
        pipeline_path,
        tx.clone(),
        rx,
        cmd_tx,
    )?;
    let _ = tx.send(UiEvent::PipelineFinished);
    runner.run()
}

pub fn page_text_with_pager(content: &str) -> Result<()> {
    state::page_text_with_pager(content)
}
