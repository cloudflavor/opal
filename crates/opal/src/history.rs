use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub run_id: String,
    pub finished_at: String,
    pub status: HistoryStatus,
    #[serde(default)]
    pub scope_root: Option<String>,
    #[serde(default)]
    pub ref_name: Option<String>,
    #[serde(default)]
    pub pipeline_file: Option<String>,
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
    #[serde(default)]
    pub artifact_dir: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub caches: Vec<HistoryCache>,
    #[serde(default)]
    pub container_name: Option<String>,
    #[serde(default)]
    pub service_network: Option<String>,
    #[serde(default)]
    pub service_containers: Vec<String>,
    #[serde(default)]
    pub runtime_summary_path: Option<String>,
    #[serde(default)]
    pub env_vars: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryCache {
    pub key: String,
    pub policy: String,
    pub host: String,
    #[serde(default)]
    pub paths: Vec<String>,
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

pub async fn load_async(path: &Path) -> Result<Vec<HistoryEntry>> {
    let contents = match tokio::fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {:?}", path));
        }
    };
    let entries: Vec<HistoryEntry> = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse history {:?}", path))?;
    Ok(entries)
}

pub fn save(path: &Path, entries: &[HistoryEntry]) -> Result<()> {
    let serialized =
        serde_json::to_string_pretty(entries).context("failed to serialize history")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create history dir {:?}", parent))?;
    }
    fs::write(path, serialized).with_context(|| format!("failed to write {:?}", path))
}

pub async fn save_async(path: &Path, entries: &[HistoryEntry]) -> Result<()> {
    let serialized =
        serde_json::to_string_pretty(entries).context("failed to serialize history")?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create history dir {:?}", parent))?;
    }
    tokio::fs::write(path, serialized)
        .await
        .with_context(|| format!("failed to write {:?}", path))
}
