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
    JobYaml,
}

#[derive(Clone)]
pub enum HistoryAction {
    SelectJob(usize),
    ViewLog { title: String, path: PathBuf },
    ViewRun(String),
    ViewHistoryJob { run_id: String, job_name: String },
    ViewDir { title: String, path: PathBuf },
    ViewFile { title: String, path: PathBuf },
}

#[derive(Clone)]
pub struct UiJobInfo {
    pub name: String,
    pub source_name: String,
    pub stage: String,
    pub log_path: PathBuf,
    pub log_hash: String,
    pub runner: UiRunnerInfo,
}

#[derive(Clone, Default)]
pub struct UiRunnerInfo {
    pub engine: String,
    pub arch: Option<String>,
    pub cpus: Option<String>,
    pub memory: Option<String>,
}

#[derive(Clone, Default)]
pub struct UiJobResources {
    pub artifact_dir: Option<String>,
    pub artifact_paths: Vec<String>,
    pub caches: Vec<HistoryCache>,
    pub container_name: Option<String>,
    pub service_network: Option<String>,
    pub service_containers: Vec<String>,
    pub runtime_summary_path: Option<String>,
}

impl From<&HistoryJob> for UiJobResources {
    fn from(job: &HistoryJob) -> Self {
        Self {
            artifact_dir: job.artifact_dir.clone(),
            artifact_paths: job.artifacts.clone(),
            caches: job.caches.clone(),
            container_name: job.container_name.clone(),
            service_network: job.service_network.clone(),
            service_containers: job.service_containers.clone(),
            runtime_summary_path: job.runtime_summary_path.clone(),
        }
    }
}

#[derive(Clone)]
pub enum UiCommand {
    RestartJob { name: String },
    StartManual { name: String },
    CancelJob { name: String },
    AnalyzeJob { name: String, source_name: String },
    PreviewAiPrompt { name: String, source_name: String },
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
    AnalysisStarted {
        name: String,
        provider: String,
    },
    AnalysisChunk {
        name: String,
        delta: String,
    },
    AnalysisFinished {
        name: String,
        final_text: String,
        saved_path: Option<PathBuf>,
        error: Option<String>,
    },
    AiPromptReady {
        name: String,
        prompt: String,
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
