use crate::history::{HistoryCache, HistoryEntry, HistoryJob, HistoryStatus};
use std::path::PathBuf;

pub const LOG_SCROLL_STEP: usize = 3;
pub const LOG_SCROLL_HALF: usize = 20;
pub const LOG_SCROLL_PAGE: usize = 60;
pub const CURRENT_HISTORY_KEY: &str = "__current_run__";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    History,
    Jobs,
}

#[derive(Clone)]
pub enum HistoryAction {
    SelectJob(usize),
    ViewLog { title: String, path: PathBuf },
    ViewRun(String),
    ViewDir { title: String, path: PathBuf },
    ViewFile { title: String, path: PathBuf },
}

#[derive(Clone)]
pub struct UiJobInfo {
    pub name: String,
    pub stage: String,
    pub log_path: PathBuf,
    pub log_hash: String,
}

#[derive(Clone, Default)]
pub struct UiJobResources {
    pub artifact_dir: Option<String>,
    pub artifact_paths: Vec<String>,
    pub caches: Vec<HistoryCache>,
}

impl From<&HistoryJob> for UiJobResources {
    fn from(job: &HistoryJob) -> Self {
        Self {
            artifact_dir: job.artifact_dir.clone(),
            artifact_paths: job.artifacts.clone(),
            caches: job.caches.clone(),
        }
    }
}

#[derive(Clone)]
pub enum UiCommand {
    RestartJob { name: String },
    StartManual { name: String },
    CancelJob { name: String },
    AbortPipeline,
}

#[derive(Clone)]
pub enum UiEvent {
    JobStarted {
        name: String,
    },
    JobRestarted {
        name: String,
    },
    JobLog {
        name: String,
        line: String,
    },
    JobFinished {
        name: String,
        status: UiJobStatus,
        duration: f32,
        error: Option<String>,
    },
    JobManual {
        name: String,
    },
    HistoryUpdated {
        entry: HistoryEntry,
    },
    PipelineFinished,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UiJobStatus {
    Pending,
    Running,
    Success,
    Failed,
    Skipped,
}

impl UiJobStatus {
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            UiJobStatus::Success | UiJobStatus::Failed | UiJobStatus::Skipped
        )
    }

    pub fn is_restartable(self) -> bool {
        matches!(self, UiJobStatus::Success | UiJobStatus::Failed)
    }

    pub fn label(self) -> &'static str {
        match self {
            UiJobStatus::Pending => "pending",
            UiJobStatus::Running => "running",
            UiJobStatus::Success => "success",
            UiJobStatus::Failed => "failed",
            UiJobStatus::Skipped => "skipped",
        }
    }

    pub fn from_history(status: HistoryStatus) -> Self {
        match status {
            HistoryStatus::Success => UiJobStatus::Success,
            HistoryStatus::Failed => UiJobStatus::Failed,
            HistoryStatus::Skipped => UiJobStatus::Skipped,
            HistoryStatus::Running => UiJobStatus::Running,
        }
    }
}
