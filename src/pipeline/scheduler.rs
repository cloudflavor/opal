use crate::executor::core::ExecutorCore;
use crate::ui::{UiBridge, UiJobStatus};
use anyhow::anyhow;
use humantime;
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};
use tokio::task;
use tokio::time;

use super::planner::{JobEvent, JobRunInfo, PlannedJob};

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
    let timeout = planned.timeout;
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
                    cancelled: false,
                });
                return;
            }
        };

        let exec_clone = exec.clone();
        let planned_job = planned;
        let run_info = run_info;
        let kill_info = run_info.container_name.clone();
        let ui_clone = ui.clone();
        let blocking = task::spawn_blocking(move || {
            exec_clone.run_planned_job(planned_job, run_info, ui_clone)
        });
        let event = if let Some(limit) = timeout {
            match time::timeout(limit, blocking).await {
                Ok(result) => match result {
                    Ok(event) => event,
                    Err(err) => JobEvent {
                        name: job_name.clone(),
                        stage_name: stage_name.clone(),
                        duration: 0.0,
                        log_path: Some(log_path.clone()),
                        log_hash: log_hash.clone(),
                        result: Err(anyhow!("job task panicked: {err}")),
                        cancelled: false,
                    },
                },
                Err(_) => {
                    exec.kill_container(&job_name, &kill_info);
                    JobEvent {
                        name: job_name.clone(),
                        stage_name: stage_name.clone(),
                        duration: limit.as_secs_f32(),
                        log_path: Some(log_path.clone()),
                        log_hash: log_hash.clone(),
                        result: Err(anyhow!(
                            "job exceeded timeout of {}",
                            humantime::format_duration(limit)
                        )),
                        cancelled: false,
                    }
                }
            }
        } else {
            match blocking.await {
                Ok(event) => event,
                Err(err) => JobEvent {
                    name: job_name.clone(),
                    stage_name: stage_name.clone(),
                    duration: 0.0,
                    log_path: Some(log_path.clone()),
                    log_hash: log_hash.clone(),
                    result: Err(anyhow!("job task panicked: {err}")),
                    cancelled: false,
                },
            }
        };
        if let Some(ui) = &ui
            && event.result.is_err()
            && !event.cancelled
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
