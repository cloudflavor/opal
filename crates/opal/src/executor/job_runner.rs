use super::core::ExecutorCore;
use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::pipeline::{JobEvent, JobFailureKind, JobRunInfo};
use crate::runner::ExecuteContext;
use crate::ui::{UiBridge, UiJobStatus};
use anyhow::{Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Handle;

struct JobRunRequest<'a> {
    exec: &'a ExecutorCore,
    runtime_handle: &'a Handle,
    plan: &'a ExecutionPlan,
    job: &'a crate::model::JobSpec,
    stage_name: &'a str,
    log_path: &'a Path,
    run_info: &'a JobRunInfo,
    job_start: Instant,
    ui: Option<&'a UiBridge>,
}

#[derive(Debug, PartialEq, Eq)]
struct RuntimeObjectsData {
    service_network: Option<String>,
    service_containers: Vec<String>,
}

pub(crate) fn run_planned_job(
    exec: &ExecutorCore,
    runtime_handle: &Handle,
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

    let result = execute_job_run(JobRunRequest {
        exec,
        runtime_handle,
        plan: plan.as_ref(),
        job: &job,
        stage_name: &stage_name,
        log_path: &log_path,
        run_info: &run_info,
        job_start,
        ui: ui_ref,
    });

    let duration = job_start.elapsed().as_secs_f32();
    let cancelled = exec.take_cancelled_job(&job_name);
    let final_result = completion_result(result, cancelled, &log_path);
    report_ui_completion(ui_ref, &job_name, &final_result, cancelled, duration);

    exec.clear_running_container(&job_name);

    build_job_event(
        job_name,
        stage_name,
        duration,
        log_path,
        log_hash,
        final_result,
        cancelled,
    )
}

fn execute_job_run(request: JobRunRequest<'_>) -> Result<()> {
    let JobRunRequest {
        exec,
        runtime_handle,
        plan,
        job,
        stage_name,
        log_path,
        run_info,
        job_start,
        ui,
    } = request;

    let mut prepared = runtime_handle.block_on(exec.prepare_job_run(plan, job))?;
    let container_name = run_info.container_name.clone();
    let exec_result = exec.execute(execute_context(
        &prepared,
        job,
        &container_name,
        log_path,
        ui,
        exec.config.settings.preserve_runtime_objects(),
    ));

    record_runtime_objects(exec, job, &container_name, &prepared)?;
    cleanup_runtime(exec, runtime_handle, &container_name, &mut prepared);
    collect_job_artifacts(exec, job, &prepared)?;
    exec_result?;
    exec.print_job_completion(
        stage_name,
        &prepared.script_path,
        log_path,
        job_start.elapsed().as_secs_f32(),
    );
    Ok(())
}

fn execute_context<'a>(
    prepared: &'a crate::executor::core::PreparedJobRun,
    job: &'a crate::model::JobSpec,
    container_name: &'a str,
    log_path: &'a Path,
    ui: Option<&'a UiBridge>,
    preserve_runtime_objects: bool,
) -> ExecuteContext<'a> {
    ExecuteContext {
        host_workdir: &prepared.host_workdir,
        script_path: &prepared.script_path,
        log_path,
        mounts: &prepared.mounts,
        image: &prepared.job_image.name,
        image_platform: prepared.job_image.docker_platform.as_deref(),
        image_user: prepared.job_image.docker_user.as_deref(),
        image_entrypoint: &prepared.job_image.entrypoint,
        container_name,
        job,
        ui,
        env_vars: &prepared.env_vars,
        network: prepared
            .service_runtime
            .as_ref()
            .map(|runtime| runtime.network_name()),
        preserve_runtime_objects,
        arch: prepared.arch.as_deref(),
        privileged: prepared.privileged,
        cap_add: &prepared.cap_add,
        cap_drop: &prepared.cap_drop,
    }
}

fn record_runtime_objects(
    exec: &ExecutorCore,
    job: &crate::model::JobSpec,
    container_name: &str,
    prepared: &crate::executor::core::PreparedJobRun,
) -> Result<()> {
    let runtime_data = runtime_objects_data(
        prepared
            .service_runtime
            .as_ref()
            .map(|runtime| runtime.network_name()),
        prepared
            .service_runtime
            .as_ref()
            .map(|runtime| runtime.container_names()),
    );
    let runtime_summary_path = exec.write_runtime_summary(
        &job.name,
        container_name,
        runtime_data.service_network.as_deref(),
        &runtime_data.service_containers,
    )?;
    exec.record_runtime_objects(
        &job.name,
        container_name.to_string(),
        runtime_data.service_network,
        runtime_data.service_containers,
        runtime_summary_path,
    );
    Ok(())
}

fn runtime_objects_data(
    service_network: Option<&str>,
    service_containers: Option<&[String]>,
) -> RuntimeObjectsData {
    RuntimeObjectsData {
        service_network: service_network.map(str::to_string),
        service_containers: service_containers.map_or_else(Vec::new, |names| names.to_vec()),
    }
}

fn cleanup_runtime(
    exec: &ExecutorCore,
    runtime_handle: &Handle,
    container_name: &str,
    prepared: &mut crate::executor::core::PreparedJobRun,
) {
    if !exec.config.settings.preserve_runtime_objects() {
        exec.cleanup_finished_container(container_name);
    }
    if let Some(mut runtime) = prepared.service_runtime.take()
        && !exec.config.settings.preserve_runtime_objects()
    {
        runtime_handle.block_on(runtime.cleanup());
    }
}

fn collect_job_artifacts(
    exec: &ExecutorCore,
    job: &crate::model::JobSpec,
    prepared: &crate::executor::core::PreparedJobRun,
) -> Result<()> {
    exec.collect_declared_artifacts(job, &prepared.host_workdir, &prepared.mounts)?;
    exec.collect_untracked_artifacts(job, &prepared.host_workdir)?;
    exec.collect_dotenv_artifacts(job, &prepared.host_workdir, &prepared.mounts)?;
    Ok(())
}

fn report_ui_completion(
    ui: Option<&UiBridge>,
    job_name: &str,
    result: &Result<()>,
    cancelled: bool,
    duration: f32,
) {
    let Some(ui) = ui else {
        return;
    };
    let (status, detail) = ui_completion(result, cancelled);
    ui.job_finished(job_name, status, duration, detail);
}

fn ui_completion(result: &Result<()>, cancelled: bool) -> (UiJobStatus, Option<String>) {
    if result.is_ok() {
        return (UiJobStatus::Success, None);
    }
    if cancelled {
        return (UiJobStatus::Skipped, Some("aborted by user".to_string()));
    }
    (
        UiJobStatus::Failed,
        result.as_ref().err().map(|err| err.to_string()),
    )
}

fn build_job_event(
    job_name: String,
    stage_name: String,
    duration: f32,
    log_path: PathBuf,
    log_hash: String,
    result: Result<()>,
    cancelled: bool,
) -> JobEvent {
    let exit_code = extract_exit_code(&result, cancelled);
    let failure_kind = classify_failure(&job_name, &result, cancelled);

    JobEvent {
        name: job_name,
        stage_name,
        duration,
        log_path: Some(log_path),
        log_hash,
        result,
        failure_kind,
        exit_code,
        cancelled,
    }
}

fn completion_result(result: Result<()>, cancelled: bool, log_path: &Path) -> Result<()> {
    if cancelled {
        Err(anyhow!("job cancelled by user"))
    } else {
        enrich_failure_with_log_hint(result, log_path)
    }
}

fn enrich_failure_with_log_hint(result: Result<()>, log_path: &Path) -> Result<()> {
    let err = match result {
        Ok(()) => return Ok(()),
        Err(err) => err,
    };
    let message = err.to_string();
    if !message.contains("container command exited with status") {
        return Err(err);
    }
    let Some(hint) = failure_hint_from_log(log_path) else {
        return Err(err);
    };
    Err(anyhow!("{message}; hint: {hint}"))
}

fn failure_hint_from_log(log_path: &Path) -> Option<&'static str> {
    let tail = read_log_tail(log_path, 32 * 1024)?;
    failure_hint_from_text(&tail)
}

fn failure_hint_from_text(tail: &str) -> Option<&'static str> {
    if tail.contains("error: rustup could not choose a version of rustc to run")
        && tail.contains("no default is configured")
    {
        return Some(
            "the job log shows an empty rustup home. A workspace-local `RUSTUP_HOME` can mask the Rust toolchain bundled in the image, especially after a cold branch/tag cache key. Prefer leaving `RUSTUP_HOME` unset in Rust images, or bootstrap the toolchain explicitly before running `rustc`/`cargo`",
        );
    }
    None
}

fn read_log_tail(log_path: &Path, max_bytes: usize) -> Option<String> {
    let bytes = fs::read(log_path).ok()?;
    let start = bytes.len().saturating_sub(max_bytes);
    Some(String::from_utf8_lossy(&bytes[start..]).into_owned())
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
    use super::{
        build_job_event, classify_failure, completion_result, execute_context, extract_exit_code,
        failure_hint_from_text, runtime_objects_data, ui_completion,
    };
    use crate::model::{ArtifactSpec, ImageSpec, JobSpec, RetryPolicySpec};
    use crate::pipeline::JobFailureKind;
    use crate::pipeline::VolumeMount;
    use crate::ui::UiJobStatus;
    use anyhow::anyhow;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn completion_result_prefers_cancelled_state() {
        let log_path = temp_path("job-run-cancelled").join("job.log");
        let result =
            completion_result(Ok(()), true, &log_path).expect_err("cancelled job should fail");
        assert_eq!(result.to_string(), "job cancelled by user");
    }

    #[test]
    fn failure_hint_from_text_detects_rustup_hint() {
        let hint = failure_hint_from_text(
            "[fetch-sources] error: rustup could not choose a version of rustc to run, because one wasn't specified explicitly, and no default is configured.\nhelp: run 'rustup default stable' to download the latest stable release of Rust and set it as your default toolchain.\n",
        );

        let hint = hint.expect("rustup failure should produce a hint");
        assert!(hint.contains("RUSTUP_HOME"));
        assert!(hint.contains("cold branch/tag cache key"));
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

    #[test]
    fn ui_completion_marks_cancelled_jobs_as_skipped() {
        let result = Err(anyhow!("job cancelled by user"));
        let (status, detail) = ui_completion(&result, true);
        assert!(matches!(status, UiJobStatus::Skipped));
        assert_eq!(detail.as_deref(), Some("aborted by user"));
    }

    #[test]
    fn ui_completion_surfaces_failures_for_non_cancelled_jobs() {
        let result = Err(anyhow!("container command exited with status Some(1)"));
        let (status, detail) = ui_completion(&result, false);
        assert!(matches!(status, UiJobStatus::Failed));
        assert_eq!(
            detail.as_deref(),
            Some("container command exited with status Some(1)")
        );
    }

    #[test]
    fn build_job_event_captures_failure_metadata() {
        let event = build_job_event(
            "test".to_string(),
            "stage".to_string(),
            1.5,
            PathBuf::from("/tmp/test.log"),
            "hash".to_string(),
            Err(anyhow!("container command exited with status Some(137)")),
            false,
        );

        assert_eq!(event.name, "test");
        assert_eq!(event.stage_name, "stage");
        assert_eq!(event.duration, 1.5);
        assert_eq!(event.log_path, Some(PathBuf::from("/tmp/test.log")));
        assert_eq!(event.log_hash, "hash");
        assert_eq!(event.exit_code, Some(137));
        assert_eq!(event.failure_kind, Some(JobFailureKind::ScriptFailure));
        assert!(!event.cancelled);
        assert_eq!(
            event
                .result
                .expect_err("event should preserve failure")
                .to_string(),
            "container command exited with status Some(137)"
        );
    }

    #[test]
    fn execute_context_maps_prepared_job_fields() {
        let job = job("test");
        let log_path = PathBuf::from("/tmp/test.log");
        let host_workdir = PathBuf::from("/tmp/workdir");
        let script_path = PathBuf::from("/tmp/script.sh");
        let mounts = vec![VolumeMount {
            host: PathBuf::from("/tmp/host"),
            container: PathBuf::from("/workspace/host"),
            read_only: true,
        }];
        let env_vars = vec![("KEY".to_string(), "VALUE".to_string())];
        let job_image = ImageSpec {
            name: "rust:1.85".to_string(),
            docker_platform: Some("linux/arm64".to_string()),
            docker_user: Some("1000:1000".to_string()),
            entrypoint: vec!["/bin/sh".to_string()],
        };
        let prepared = crate::executor::core::PreparedJobRun {
            host_workdir,
            env_vars,
            service_runtime: None,
            mounts,
            job_image,
            arch: Some("aarch64".to_string()),
            privileged: true,
            cap_add: vec!["NET_ADMIN".to_string()],
            cap_drop: vec!["MKNOD".to_string()],
            script_path,
        };

        let ctx = execute_context(&prepared, &job, "opal-job-01", &log_path, None, false);

        assert_eq!(ctx.host_workdir, PathBuf::from("/tmp/workdir").as_path());
        assert_eq!(ctx.script_path, PathBuf::from("/tmp/script.sh").as_path());
        assert_eq!(ctx.log_path, PathBuf::from("/tmp/test.log").as_path());
        assert_eq!(ctx.mounts.len(), 1);
        assert_eq!(ctx.image, "rust:1.85");
        assert_eq!(ctx.image_platform, Some("linux/arm64"));
        assert_eq!(ctx.image_user, Some("1000:1000"));
        assert_eq!(ctx.image_entrypoint, ["/bin/sh".to_string()]);
        assert_eq!(ctx.container_name, "opal-job-01");
        assert_eq!(ctx.job.name, "test");
        assert_eq!(ctx.env_vars, [("KEY".to_string(), "VALUE".to_string())]);
        assert_eq!(ctx.network, None);
        assert!(!ctx.preserve_runtime_objects);
        assert_eq!(ctx.arch, Some("aarch64"));
        assert!(ctx.privileged);
        assert_eq!(ctx.cap_add, ["NET_ADMIN".to_string()]);
        assert_eq!(ctx.cap_drop, ["MKNOD".to_string()]);
    }

    #[test]
    fn runtime_objects_data_copies_runtime_metadata() {
        let containers = vec!["svc-db".to_string(), "svc-cache".to_string()];
        let data = runtime_objects_data(Some("opal-net"), Some(&containers));

        assert_eq!(data.service_network.as_deref(), Some("opal-net"));
        assert_eq!(data.service_containers, containers);
    }

    #[test]
    fn runtime_objects_data_defaults_when_no_services_exist() {
        let data = runtime_objects_data(None, None);

        assert_eq!(data.service_network, None);
        assert!(data.service_containers.is_empty());
    }

    fn temp_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("opal-{prefix}-{nanos}"))
    }

    fn job(name: &str) -> JobSpec {
        JobSpec {
            name: name.into(),
            stage: "test".into(),
            commands: vec!["true".into()],
            needs: Vec::new(),
            explicit_needs: false,
            dependencies: Vec::new(),
            before_script: None,
            after_script: None,
            inherit_default_image: true,
            inherit_default_before_script: true,
            inherit_default_after_script: true,
            inherit_default_cache: true,
            inherit_default_services: true,
            inherit_default_timeout: true,
            inherit_default_retry: true,
            inherit_default_interruptible: true,
            when: None,
            rules: Vec::new(),
            only: Vec::new(),
            except: Vec::new(),
            artifacts: ArtifactSpec::default(),
            cache: Vec::new(),
            image: None,
            variables: HashMap::new(),
            services: Vec::new(),
            timeout: None,
            retry: RetryPolicySpec::default(),
            interruptible: false,
            resource_group: None,
            parallel: None,
            tags: Vec::new(),
            environment: None,
        }
    }
}
