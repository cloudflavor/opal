use crate::model::EnvironmentSpec;
use anyhow::Result;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct JobSummary {
    pub name: String,
    pub stage_name: String,
    pub duration: f32,
    pub status: JobStatus,
    pub log_path: Option<PathBuf>,
    pub log_hash: String,
    pub allow_failure: bool,
    pub environment: Option<EnvironmentSpec>,
}

#[derive(Debug, Clone)]
pub enum JobStatus {
    Success,
    Failed(String),
    Skipped(String),
}

#[derive(Debug, Clone)]
pub struct JobRunInfo {
    pub container_name: String,
}

#[derive(Debug)]
pub struct JobEvent {
    pub name: String,
    pub stage_name: String,
    pub duration: f32,
    pub log_path: Option<PathBuf>,
    pub log_hash: String,
    pub result: Result<()>,
    pub failure_kind: Option<JobFailureKind>,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobFailureKind {
    UnknownFailure,
    ScriptFailure,
    ApiFailure,
    JobExecutionTimeout,
    RunnerUnsupported,
    StaleSchedule,
    ArchivedFailure,
    UnmetPrerequisites,
    SchedulerFailure,
    DataIntegrityFailure,
    RunnerSystemFailure,
    StuckOrTimeoutFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltKind {
    None,
    JobFailure,
    Deadlock,
    ChannelClosed,
    Aborted,
}

#[derive(Debug, Clone)]
pub struct StageState {
    pub total: usize,
    pub completed: usize,
    pub header_printed: bool,
    pub started_at: Option<Instant>,
}

impl StageState {
    pub fn new(total: usize) -> Self {
        Self {
            total,
            completed: 0,
            header_printed: false,
            started_at: None,
        }
    }
}
