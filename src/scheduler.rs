use crate::executor::core::ExecutorCore;
use crate::planner::PlannedJob;
use crate::ui::{UiBridge, UiJobStatus};
use anyhow::{Result, anyhow};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Semaphore};
use tokio::task;

pub fn spawn_job(
    exec: Arc<ExecutorCore>,
    planned: PlannedJob,
    run_info: JobRunInfo,
    semaphore: Arc<Semaphore>,
    tx: mpsc::UnboundedSender<JobEvent>,
    ui: Option<Arc<UiBridge>>,
) {
    let job_name = planned.job.name.clone();
    let stage_name = planned.stage_name.clone();
    let log_path = planned.log_path.clone();
    let log_hash = planned.log_hash.clone();
    task::spawn(async move {
        let permit = match semaphore.acquire_owned().await {
            Ok(permit) => permit,
            Err(err) => {
                if let Some(ui) = &ui {
                    ui.job_finished(
                        &job_name,
                        UiJobStatus::Failed,
                        0.0,
                        Some(format!("failed to acquire job slot: {err}")),
                    );
                }
                let _ = tx.send(JobEvent {
                    name: job_name.clone(),
                    stage_name: stage_name.clone(),
                    duration: 0.0,
                    log_path: Some(log_path.clone()),
                    log_hash: log_hash.clone(),
                    result: Err(anyhow!("failed to acquire job slot: {err}")),
                });
                return;
            }
        };

        let exec_clone = exec.clone();
        let planned_job = planned;
        let run_info = run_info;
        let ui_clone = ui.clone();
        let result = task::spawn_blocking(move || {
            exec_clone.run_planned_job(planned_job, run_info, ui_clone)
        })
        .await;
        let event = match result {
            Ok(event) => event,
            Err(err) => JobEvent {
                name: job_name.clone(),
                stage_name: stage_name.clone(),
                duration: 0.0,
                log_path: Some(log_path.clone()),
                log_hash: log_hash.clone(),
                result: Err(anyhow!("job task panicked: {err}")),
            },
        };
        if let Some(ui) = &ui
            && event.result.is_err()
        {
            ui.job_finished(
                &job_name,
                UiJobStatus::Failed,
                event.duration,
                event.result.as_ref().err().map(|e| e.to_string()),
            );
        }

        drop(permit);
        let _ = tx.send(event);
    });
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
}

#[derive(Debug, Clone)]
pub struct JobSummary {
    pub name: String,
    pub stage_name: String,
    pub duration: f32,
    pub status: JobStatus,
    pub log_path: Option<PathBuf>,
    pub log_hash: String,
}

#[derive(Debug, Clone)]
pub enum JobStatus {
    Success,
    Failed(String),
    Skipped(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltKind {
    None,
    JobFailure,
    Deadlock,
    ChannelClosed,
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
