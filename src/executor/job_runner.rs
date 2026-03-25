use super::core::ExecutorCore;
use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::pipeline::{JobEvent, JobFailureKind, JobRunInfo};
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

    // TODO: what the fuck is this, why is this a function here?
    // the logic needs to be simplified, garbage
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
        exec.collect_untracked_artifacts(&job, &prepared.host_workdir)?;
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

    let exit_code = extract_exit_code(&final_result, cancelled);
    let failure_kind = classify_failure(&job_name, &final_result, cancelled);

    JobEvent {
        name: job_name,
        stage_name,
        duration,
        log_path: Some(log_path.clone()),
        log_hash,
        result: final_result,
        failure_kind,
        exit_code,
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

fn extract_exit_code(result: &Result<()>, cancelled: bool) -> Option<i32> {
    if cancelled {
        return None;
    }
    let message = result.as_ref().err()?.to_string();
    let marker = "container command exited with status Some(";
    let start = message.find(marker)? + marker.len();
    let rest = &message[start..];
    let end = rest.find(')')?;
    rest[..end].parse::<i32>().ok()
}

fn classify_failure(
    job_name: &str,
    result: &Result<()>,
    cancelled: bool,
) -> Option<JobFailureKind> {
    if cancelled {
        return None;
    }
    let err = result.as_ref().err()?;
    let message = err.to_string();
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("job exceeded timeout") {
        return Some(JobFailureKind::JobExecutionTimeout);
    }
    if is_api_failure(&normalized) {
        return Some(JobFailureKind::ApiFailure);
    }
    if is_runner_unsupported(&normalized) {
        return Some(JobFailureKind::RunnerUnsupported);
    }
    if is_stale_schedule_failure(&normalized) {
        return Some(JobFailureKind::StaleSchedule);
    }
    if is_archived_failure(&normalized) {
        return Some(JobFailureKind::ArchivedFailure);
    }
    if is_unmet_prerequisites(&normalized) {
        return Some(JobFailureKind::UnmetPrerequisites);
    }
    if is_scheduler_failure(&normalized) {
        return Some(JobFailureKind::SchedulerFailure);
    }
    if is_runner_system_failure(&normalized) {
        return Some(JobFailureKind::RunnerSystemFailure);
    }
    if is_stuck_or_timeout_failure(&normalized) {
        return Some(JobFailureKind::StuckOrTimeoutFailure);
    }
    if is_data_integrity_failure(&normalized) {
        return Some(JobFailureKind::DataIntegrityFailure);
    }
    if is_script_failure(&normalized) {
        return Some(JobFailureKind::ScriptFailure);
    }
    let _ = job_name;
    Some(JobFailureKind::UnknownFailure)
}

fn is_api_failure(message: &str) -> bool {
    message.contains("failed to invoke curl to download artifacts")
        || message.contains("curl failed to download artifacts")
        || message.contains("failed to download artifacts for")
}

fn is_runner_unsupported(message: &str) -> bool {
    message.contains("services are only supported when using")
}

fn is_stale_schedule_failure(message: &str) -> bool {
    message.contains("stale schedule")
}

fn is_archived_failure(message: &str) -> bool {
    message.contains("archived failure")
        || message.contains("project is archived")
        || message.contains("repository is archived")
}

fn is_unmet_prerequisites(message: &str) -> bool {
    message.contains("has no image")
        || message.contains("requires artifacts from project")
        || message.contains("requires artifacts from '")
        || message.contains("but it did not run")
        || message.contains("no gitlab token is configured")
        || message.contains("depends on unknown job")
}

fn is_scheduler_failure(message: &str) -> bool {
    message.contains("failed to acquire job slot")
}

fn is_runner_system_failure(message: &str) -> bool {
    message.contains("failed to start service")
        || message.contains("failed readiness check")
        || message.contains("failed to create network")
        || message.contains("job task panicked")
        || message.contains("failed to run docker command")
        || message.contains("failed to run podman command")
        || message.contains("failed to run nerdctl command")
        || message.contains("failed to run containercli command")
        || message.contains("failed to run orbstack command")
        || message.contains("missing stdout from container process")
        || message.contains("missing stderr from container process")
        || message.contains("failed to create log at")
        || message.contains("failed to invoke python3 to extract artifacts")
}

fn is_stuck_or_timeout_failure(message: &str) -> bool {
    message.contains("timed out")
}

fn is_data_integrity_failure(message: &str) -> bool {
    message.contains("unable to extract artifacts archive")
}

fn is_script_failure(message: &str) -> bool {
    message.contains("container command exited with status")
}

#[cfg(test)]
mod tests {
    use super::{classify_failure, completion_result, extract_exit_code};
    use crate::pipeline::JobFailureKind;
    use anyhow::anyhow;

    #[test]
    fn completion_result_prefers_cancelled_state() {
        let result = completion_result(Ok(()), true).expect_err("cancelled job should fail");
        assert_eq!(result.to_string(), "job cancelled by user");
    }

    #[test]
    fn classify_failure_distinguishes_timeout() {
        let result = Err(anyhow!("job exceeded timeout of 5m"));
        assert_eq!(
            classify_failure("job", &result, false),
            Some(JobFailureKind::JobExecutionTimeout)
        );
    }

    #[test]
    fn classify_failure_defaults_to_script_failure() {
        let result = Err(anyhow!("container command exited with status Some(1)"));
        assert_eq!(
            classify_failure("job", &result, false),
            Some(JobFailureKind::ScriptFailure)
        );
    }

    #[test]
    fn classify_failure_detects_api_failure() {
        let result = Err(anyhow!(
            "failed to download artifacts for 'build' from project 'group/project': curl failed to download artifacts from https://gitlab.example/api/v4/projects/group%2Fproject/jobs/artifacts/main/download?job=build (status 404)"
        ));
        assert_eq!(
            classify_failure("job", &result, false),
            Some(JobFailureKind::ApiFailure)
        );
    }

    #[test]
    fn classify_failure_detects_unmet_prerequisites() {
        let result = Err(anyhow!(
            "job 'build' has no image (use --base-image or set image in pipeline/job)"
        ));
        assert_eq!(
            classify_failure("job", &result, false),
            Some(JobFailureKind::UnmetPrerequisites)
        );
    }

    #[test]
    fn classify_failure_falls_back_to_unknown_failure() {
        let result = Err(anyhow!("unexpected executor error"));
        assert_eq!(
            classify_failure("job", &result, false),
            Some(JobFailureKind::UnknownFailure)
        );
    }

    #[test]
    fn classify_failure_detects_stale_schedule() {
        let result = Err(anyhow!("stale schedule prevented delayed job execution"));
        assert_eq!(
            classify_failure("job", &result, false),
            Some(JobFailureKind::StaleSchedule)
        );
    }

    #[test]
    fn classify_failure_detects_archived_failure() {
        let result = Err(anyhow!("project is archived"));
        assert_eq!(
            classify_failure("job", &result, false),
            Some(JobFailureKind::ArchivedFailure)
        );
    }

    #[test]
    fn classify_failure_detects_data_integrity_failure() {
        let result = Err(anyhow!(
            "unable to extract artifacts archive /tmp/archive.zip"
        ));
        assert_eq!(
            classify_failure("job", &result, false),
            Some(JobFailureKind::DataIntegrityFailure)
        );
    }

    #[test]
    fn extract_exit_code_reads_container_exit_status() {
        let result = Err(anyhow!("container command exited with status Some(137)"));
        assert_eq!(extract_exit_code(&result, false), Some(137));
    }
}
