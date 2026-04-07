use crate::pipeline::JobFailureKind;

pub(super) fn retry_allowed(
    conditions: &[String],
    exit_codes: &[i32],
    failure_kind: Option<JobFailureKind>,
    exit_code: Option<i32>,
) -> bool {
    if conditions.is_empty() && exit_codes.is_empty() {
        return true;
    }
    let when_matches = failure_kind.is_some_and(|kind| {
        conditions
            .iter()
            .any(|condition| retry_condition_matches(condition, kind))
    });
    let exit_code_matches = exit_code.is_some_and(|code| exit_codes.contains(&code));
    when_matches || exit_code_matches
}

fn retry_condition_matches(condition: &str, failure_kind: JobFailureKind) -> bool {
    match condition {
        "always" => true,
        "unknown_failure" => failure_kind == JobFailureKind::UnknownFailure,
        "script_failure" => failure_kind == JobFailureKind::ScriptFailure,
        "api_failure" => failure_kind == JobFailureKind::ApiFailure,
        "job_execution_timeout" => failure_kind == JobFailureKind::JobExecutionTimeout,
        "runner_system_failure" => failure_kind == JobFailureKind::RunnerSystemFailure,
        "runner_unsupported" => failure_kind == JobFailureKind::RunnerUnsupported,
        "stale_schedule" => failure_kind == JobFailureKind::StaleSchedule,
        "archived_failure" => failure_kind == JobFailureKind::ArchivedFailure,
        "unmet_prerequisites" => failure_kind == JobFailureKind::UnmetPrerequisites,
        "scheduler_failure" => failure_kind == JobFailureKind::SchedulerFailure,
        "data_integrity_failure" => failure_kind == JobFailureKind::DataIntegrityFailure,
        "stuck_or_timeout_failure" => {
            matches!(
                failure_kind,
                JobFailureKind::StuckOrTimeoutFailure | JobFailureKind::JobExecutionTimeout
            )
        }
        _ => false,
    }
}
