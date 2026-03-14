use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub run_id: String,
    pub finished_at: String,
    pub status: HistoryStatus,
    pub jobs: Vec<HistoryJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryJob {
    pub name: String,
    pub stage: String,
    pub status: HistoryStatus,
    pub log_hash: String,
    #[serde(default)]
    pub log_path: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HistoryStatus {
    Success,
    Failed,
    Skipped,
    Running,
}

pub fn load(path: &Path) -> Result<Vec<HistoryEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {:?}", path))?;
    let entries: Vec<HistoryEntry> = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse history {:?}", path))?;
    Ok(entries)
}

pub fn save(path: &Path, entries: &[HistoryEntry]) -> Result<()> {
    let serialized =
        serde_json::to_string_pretty(entries).context("failed to serialize history")?;
    fs::write(path, serialized).with_context(|| format!("failed to write {:?}", path))
}
