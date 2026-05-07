use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::executor::{core::ExecutorCore, job_runner};
use crate::ui::{UiBridge, UiJobStatus};
use anyhow::anyhow;
use humantime;
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};
use tokio::task;
use tokio::time;

use super::planner::{JobEvent, JobFailureKind, JobRunInfo};

pub fn spawn_job(
    exec: Arc<ExecutorCore>,
    plan: Arc<ExecutionPlan>,
    planned: ExecutableJob,
    run_info: JobRunInfo,
    semaphore: Arc<Semaphore>,
    tx: mpsc::UnboundedSender<JobEvent>,
    ui: Option<Arc<UiBridge>>,
) {
    let job_name = planned.instance.job.name.clone();
    let stage_name = planned.instance.stage_name.clone();
    let log_path = planned.log_path.clone();
    let log_hash = planned.log_hash.clone();
    let timeout = planned.instance.timeout;

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
                    failure_kind: Some(JobFailureKind::SchedulerFailure),
                    exit_code: None,
                    cancelled: false,
                });
                return;
            }
        };

        let event = if let Some(limit) = timeout {
            match time::timeout(
                limit,
                job_runner::run_planned_job(
                    exec.as_ref(),
                    plan.clone(),
                    planned,
                    run_info,
                    ui.clone(),
                ),
            )
            .await
            {
                Ok(event) => event,
                Err(_) => {
                    let _ = exec.cancel_running_job(&job_name).await;
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
                        failure_kind: Some(JobFailureKind::JobExecutionTimeout),
                        exit_code: None,
                        cancelled: false,
                    }
                }
            }
        } else {
            job_runner::run_planned_job(exec.as_ref(), plan.clone(), planned, run_info, ui.clone())
                .await
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
