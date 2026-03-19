use super::core::ExecutorCore;
use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::pipeline::{JobEvent, JobRunInfo};
use crate::runner::ExecuteContext;
use crate::ui::{UiBridge, UiJobStatus};
use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::time::Instant;

pub(crate) fn run_planned_job(
    exec: &ExecutorCore,
    plan: Arc<ExecutionPlan>,
    planned: ExecutableJob,
    run_info: JobRunInfo,
    ui: Option<Arc<UiBridge>>,
) -> JobEvent {
    let ExecutableJob {
        instance,
        log_path,
        log_hash,
    } = planned;
    let job = instance.job;
    let stage_name = instance.stage_name;
    let job_name = job.name.clone();
    let job_start = Instant::now();
    let ui_ref = ui.as_deref();

    let result = (|| -> Result<()> {
        let mut prepared = exec.prepare_job_run(plan.as_ref(), &job)?;
        let container_name = run_info.container_name.clone();
        let exec_result = exec.execute(ExecuteContext {
            host_workdir: &prepared.host_workdir,
            script_path: &prepared.script_path,
            log_path: &log_path,
            mounts: &prepared.mounts,
            image: &prepared.job_image,
            container_name: &container_name,
            job: &job,
            ui: ui_ref,
            env_vars: &prepared.env_vars,
            network: prepared
                .service_runtime
                .as_ref()
                .map(|runtime| runtime.network_name()),
        });
        exec.cleanup_finished_container(&container_name);
        if let Some(mut runtime) = prepared.service_runtime.take() {
            runtime.cleanup();
        }
        exec_result?;
        exec.print_job_completion(
            &stage_name,
            &prepared.script_path,
            &log_path,
            job_start.elapsed().as_secs_f32(),
        );
        Ok(())
    })();

    let duration = job_start.elapsed().as_secs_f32();
    let cancelled = exec.take_cancelled_job(&job_name);
    let final_result = completion_result(result, cancelled);
    if let Some(ui) = ui_ref {
        match &final_result {
            Ok(_) => ui.job_finished(&job_name, UiJobStatus::Success, duration, None),
            Err(err) => {
                if cancelled {
                    ui.job_finished(
                        &job_name,
                        UiJobStatus::Skipped,
                        duration,
                        Some("aborted by user".to_string()),
                    );
                } else {
                    ui.job_finished(
                        &job_name,
                        UiJobStatus::Failed,
                        duration,
                        Some(err.to_string()),
                    );
                }
            }
        }
    }

    exec.clear_running_container(&job_name);

    JobEvent {
        name: job_name,
        stage_name,
        duration,
        log_path: Some(log_path.clone()),
        log_hash,
        result: final_result,
        cancelled,
    }
}

fn completion_result(result: Result<()>, cancelled: bool) -> Result<()> {
    if cancelled {
        Err(anyhow!("job cancelled by user"))
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::completion_result;

    #[test]
    fn completion_result_prefers_cancelled_state() {
        let result = completion_result(Ok(()), true).expect_err("cancelled job should fail");
        assert_eq!(result.to_string(), "job cancelled by user");
    }
}
