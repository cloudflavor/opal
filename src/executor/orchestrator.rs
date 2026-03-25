use super::{core::ExecutorCore, job_runner};
use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::model::ArtifactSourceOutcome;
use crate::pipeline::{self, HaltKind, JobEvent, JobFailureKind, JobStatus, JobSummary, RuleWhen};
use crate::ui::{UiBridge, UiCommand, UiJobStatus};
use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::{
    sync::{Semaphore, mpsc},
    task, time as tokio_time,
};

pub(crate) async fn execute_plan(
    exec: &ExecutorCore,
    plan: Arc<ExecutionPlan>,
    ui: Option<Arc<UiBridge>>,
    mut commands: Option<&mut mpsc::UnboundedReceiver<UiCommand>>,
) -> (Vec<JobSummary>, Result<()>) {
    //TODO: too complicated function, does semaphores, expects signals over channels, fucking no
    //refactor this trash and structure properly
    let total = plan.ordered.len();
    if total == 0 {
        return (Vec::new(), Ok(()));
    }

    let mut remaining: HashMap<String, usize> = plan
        .nodes
        .iter()
        .map(|(name, job)| (name.clone(), job.instance.dependencies.len()))
        .collect();
    let mut ready: VecDeque<String> = VecDeque::new();
    let mut waiting_on_failure: VecDeque<String> = VecDeque::new();
    let mut delayed_pending: HashSet<String> = HashSet::new();
    let mut manual_waiting: HashSet<String> = HashSet::new();
    let mut running = HashSet::new();
    let mut abort_requested = false;
    let mut completed = 0usize;
    let mut pipeline_failed = false;
    let mut halt_kind = HaltKind::None;
    let mut halt_error: Option<anyhow::Error> = None;
    let mut summaries: Vec<JobSummary> = Vec::new();
    let mut attempts: HashMap<String, u32> = HashMap::new();
    let mut resource_locks: HashMap<String, bool> = HashMap::new();
    let mut resource_waiting: HashMap<String, VecDeque<String>> = HashMap::new();
    let mut manual_input_available = commands.is_some();

    let semaphore = Arc::new(Semaphore::new(exec.config.max_parallel_jobs.max(1)));
    let exec = Arc::new(exec.clone());
    let (tx, mut rx) = mpsc::unbounded_channel::<JobEvent>();
    let (delay_tx, mut delay_rx) = mpsc::unbounded_channel::<String>();

    let enqueue_ready = |job_name: &str,
                         pipeline_failed_flag: bool,
                         ready_queue: &mut VecDeque<String>,
                         wait_failure_queue: &mut VecDeque<String>,
                         delayed_set: &mut HashSet<String>| {
        let Some(planned) = plan.nodes.get(job_name) else {
            return;
        };
        match planned.instance.rule.when {
            RuleWhen::OnFailure => {
                if pipeline_failed_flag {
                    ready_queue.push_back(job_name.to_string());
                } else {
                    wait_failure_queue.push_back(job_name.to_string());
                }
            }
            RuleWhen::Delayed => {
                if pipeline_failed_flag {
                    return;
                }
                if let Some(delay) = planned.instance.rule.start_in {
                    if delayed_set.insert(job_name.to_string()) {
                        let tx_clone = delay_tx.clone();
                        let name = job_name.to_string();
                        task::spawn(async move {
                            tokio_time::sleep(delay).await;
                            let _ = tx_clone.send(name);
                        });
                    }
                } else {
                    ready_queue.push_back(job_name.to_string());
                }
            }
            RuleWhen::Manual | RuleWhen::OnSuccess => {
                if pipeline_failed_flag && planned.instance.rule.when.requires_success() {
                    return;
                }
                ready_queue.push_back(job_name.to_string());
            }
            RuleWhen::Always => {
                ready_queue.push_back(job_name.to_string());
            }
            RuleWhen::Never => {}
        }
    };

    for name in &plan.ordered {
        if remaining.get(name).copied().unwrap_or(0) == 0 && !abort_requested {
            enqueue_ready(
                name,
                pipeline_failed,
                &mut ready,
                &mut waiting_on_failure,
                &mut delayed_pending,
            );
        }
    }

    while completed < total {
        while let Some(name) = ready.pop_front() {
            if abort_requested {
                break;
            }
            let planned = match plan.nodes.get(&name).cloned() {
                Some(job) => job,
                None => continue,
            };
            if pipeline_failed && planned.instance.rule.when.requires_success() {
                continue;
            }

            if matches!(planned.instance.rule.when, RuleWhen::Manual)
                && !planned.instance.rule.manual_auto_run
            {
                if manual_input_available {
                    if manual_waiting.insert(name.clone())
                        && let Some(ui_ref) = ui.as_deref()
                    {
                        ui_ref.job_manual_pending(&name);
                    }
                } else {
                    let reason = planned
                        .instance
                        .rule
                        .manual_reason
                        .clone()
                        .unwrap_or_else(|| "manual job not run".to_string());
                    if let Some(ui_ref) = ui.as_deref() {
                        ui_ref.job_finished(
                            &planned.instance.job.name,
                            UiJobStatus::Skipped,
                            0.0,
                            Some(reason.clone()),
                        );
                    }
                    summaries.push(JobSummary {
                        name: planned.instance.job.name.clone(),
                        stage_name: planned.instance.stage_name.clone(),
                        duration: 0.0,
                        status: JobStatus::Skipped(reason.clone()),
                        log_path: None,
                        log_hash: planned.log_hash.clone(),
                        allow_failure: planned.instance.rule.allow_failure,
                        environment: exec.expanded_environment(&planned.instance.job),
                    });
                    completed += 1;
                    release_dependents(
                        &plan,
                        &name,
                        &mut remaining,
                        abort_requested,
                        pipeline_failed,
                        &mut ReadyQueues {
                            ready: &mut ready,
                            waiting_on_failure: &mut waiting_on_failure,
                            delayed_pending: &mut delayed_pending,
                        },
                        &enqueue_ready,
                    );
                }
                continue;
            }

            if let Some(group) = &planned.instance.resource_group {
                if resource_locks.get(group).copied().unwrap_or(false) {
                    resource_waiting
                        .entry(group.clone())
                        .or_default()
                        .push_back(name.clone());
                    continue;
                }
                resource_locks.insert(group.clone(), true);
            }

            let entry = attempts.entry(name.clone()).or_insert(0);
            *entry += 1;

            let run_info = match exec.log_job_start(&planned, ui.as_deref()) {
                Ok(info) => info,
                Err(err) => {
                    summaries.push(JobSummary {
                        name: planned.instance.job.name.clone(),
                        stage_name: planned.instance.stage_name.clone(),
                        duration: 0.0,
                        status: JobStatus::Failed(err.to_string()),
                        log_path: Some(planned.log_path.clone()),
                        log_hash: planned.log_hash.clone(),
                        allow_failure: false,
                        environment: exec.expanded_environment(&planned.instance.job),
                    });
                    return (summaries, Err(err));
                }
            };
            running.insert(name.clone());
            pipeline::spawn_job(
                exec.clone(),
                plan.clone(),
                planned,
                run_info,
                semaphore.clone(),
                tx.clone(),
                ui.clone(),
            );
        }

        if completed >= total {
            break;
        }

        if running.is_empty()
            && ready.is_empty()
            && delayed_pending.is_empty()
            && pipeline_failed
            && waiting_on_failure.is_empty()
            && manual_waiting.is_empty()
        {
            break;
        }

        if running.is_empty()
            && ready.is_empty()
            && delayed_pending.is_empty()
            && !pipeline_failed
            && waiting_on_failure.is_empty()
            && manual_waiting.is_empty()
        {
            let remaining_jobs: Vec<_> = remaining
                .iter()
                .filter_map(|(name, &count)| if count > 0 { Some(name.clone()) } else { None })
                .collect();
            if !remaining_jobs.is_empty() {
                halt_kind = HaltKind::Deadlock;
                halt_error = Some(anyhow!(
                    "no runnable jobs, potential dependency cycle involving: {:?}",
                    remaining_jobs
                ));
            }
            break;
        }

        if running.is_empty()
            && ready.is_empty()
            && delayed_pending.is_empty()
            && !pipeline_failed
            && !waiting_on_failure.is_empty()
            && manual_waiting.is_empty()
        {
            break;
        }

        enum SchedulerEvent {
            Job(JobEvent),
            Delay(String),
            Command(UiCommand),
        }

        let next_event = tokio::select! {
            Some(event) = rx.recv() => Some(SchedulerEvent::Job(event)),
            Some(name) = delay_rx.recv() => Some(SchedulerEvent::Delay(name)),
            cmd = async {
                if let Some(rx) = commands.as_mut() {
                    (*rx).recv().await
                } else {
                    None
                }
            } => {
                match cmd {
                    Some(command) => Some(SchedulerEvent::Command(command)),
                    None => {
                        manual_input_available = false;
                        commands = None;
                        None
                    }
                }
            }
            else => None,
        };

        if !manual_input_available && !manual_waiting.is_empty() {
            let pending: Vec<String> = manual_waiting.drain().collect();
            for name in pending {
                if let Some(planned) = plan.nodes.get(&name) {
                    let reason = planned
                        .instance
                        .rule
                        .manual_reason
                        .clone()
                        .unwrap_or_else(|| "manual job not run".to_string());
                    if let Some(ui_ref) = ui.as_deref() {
                        ui_ref.job_finished(
                            &planned.instance.job.name,
                            UiJobStatus::Skipped,
                            0.0,
                            Some(reason.clone()),
                        );
                    }
                    summaries.push(JobSummary {
                        name: planned.instance.job.name.clone(),
                        stage_name: planned.instance.stage_name.clone(),
                        duration: 0.0,
                        status: JobStatus::Skipped(reason),
                        log_path: None,
                        log_hash: planned.log_hash.clone(),
                        allow_failure: planned.instance.rule.allow_failure,
                        environment: exec.expanded_environment(&planned.instance.job),
                    });
                    completed += 1;
                    release_dependents(
                        &plan,
                        &name,
                        &mut remaining,
                        abort_requested,
                        pipeline_failed,
                        &mut ReadyQueues {
                            ready: &mut ready,
                            waiting_on_failure: &mut waiting_on_failure,
                            delayed_pending: &mut delayed_pending,
                        },
                        &enqueue_ready,
                    );
                }
            }
        }

        let Some(event) = next_event else {
            if running.is_empty() && ready.is_empty() && delayed_pending.is_empty() {
                halt_kind = HaltKind::ChannelClosed;
                halt_error = Some(anyhow!(
                    "job worker channel closed unexpectedly while {} jobs remained",
                    total - completed
                ));
                break;
            }
            continue;
        };

        match event {
            SchedulerEvent::Delay(name) => {
                if abort_requested {
                    continue;
                }
                delayed_pending.remove(&name);
                if pipeline_failed
                    && let Some(planned) = plan.nodes.get(&name)
                    && planned.instance.rule.when.requires_success()
                {
                    continue;
                }
                ready.push_back(name);
            }
            SchedulerEvent::Command(cmd) => match cmd {
                UiCommand::StartManual { name } => {
                    if manual_waiting.remove(&name) {
                        ready.push_back(name);
                    }
                }
                UiCommand::CancelJob { name } => {
                    exec.cancel_running_job(&name);
                }
                UiCommand::AbortPipeline => {
                    abort_requested = true;
                    pipeline_failed = true;
                    halt_kind = HaltKind::Aborted;
                    if halt_error.is_none() {
                        halt_error = Some(anyhow!("pipeline aborted by user"));
                    }
                    exec.cancel_all_running_jobs();
                    ready.clear();
                    waiting_on_failure.clear();
                    delayed_pending.clear();
                    manual_waiting.clear();
                }
                UiCommand::RestartJob { .. } => {}
            },
            SchedulerEvent::Job(event) => {
                running.remove(&event.name);
                let Some(planned) = plan.nodes.get(&event.name) else {
                    let message = format!(
                        "completed job '{}' was not found in execution plan",
                        event.name
                    );
                    if !pipeline_failed {
                        pipeline_failed = true;
                        halt_kind = HaltKind::JobFailure;
                        if halt_error.is_none() {
                            halt_error = Some(anyhow!(message.clone()));
                        }
                    }
                    summaries.push(JobSummary {
                        name: event.name.clone(),
                        stage_name: event.stage_name.clone(),
                        duration: event.duration,
                        status: JobStatus::Failed(message),
                        log_path: event.log_path.clone(),
                        log_hash: event.log_hash.clone(),
                        allow_failure: false,
                        environment: None,
                    });
                    completed += 1;
                    continue;
                };
                match event.result {
                    Ok(_) => {
                        release_resource_lock(
                            planned,
                            &mut ready,
                            &mut resource_locks,
                            &mut resource_waiting,
                        );
                        release_dependents(
                            &plan,
                            &event.name,
                            &mut remaining,
                            abort_requested,
                            pipeline_failed,
                            &mut ReadyQueues {
                                ready: &mut ready,
                                waiting_on_failure: &mut waiting_on_failure,
                                delayed_pending: &mut delayed_pending,
                            },
                            &enqueue_ready,
                        );
                        summaries.push(JobSummary {
                            name: event.name.clone(),
                            stage_name: event.stage_name.clone(),
                            duration: event.duration,
                            status: JobStatus::Success,
                            log_path: event.log_path.clone(),
                            log_hash: event.log_hash.clone(),
                            allow_failure: planned.instance.rule.allow_failure,
                            environment: exec.expanded_environment(&planned.instance.job),
                        });
                        completed += 1;
                    }
                    Err(err) => {
                        if event.cancelled {
                            release_resource_lock(
                                planned,
                                &mut ready,
                                &mut resource_locks,
                                &mut resource_waiting,
                            );
                            summaries.push(JobSummary {
                                name: event.name.clone(),
                                stage_name: event.stage_name.clone(),
                                duration: event.duration,
                                status: JobStatus::Skipped("aborted by user".to_string()),
                                log_path: event.log_path.clone(),
                                log_hash: event.log_hash.clone(),
                                allow_failure: true,
                                environment: exec.expanded_environment(&planned.instance.job),
                            });
                            completed += 1;
                            continue;
                        }
                        let err_msg = err.to_string();
                        let attempts_so_far = attempts.get(&event.name).copied().unwrap_or(1);
                        let retries_used = attempts_so_far.saturating_sub(1);
                        if retries_used < planned.instance.retry.max
                            && retry_allowed(
                                &planned.instance.retry.when,
                                &planned.instance.retry.exit_codes,
                                event.failure_kind,
                                event.exit_code,
                            )
                        {
                            release_resource_lock(
                                planned,
                                &mut ready,
                                &mut resource_locks,
                                &mut resource_waiting,
                            );
                            ready.push_back(event.name.clone());
                            continue;
                        }
                        release_resource_lock(
                            planned,
                            &mut ready,
                            &mut resource_locks,
                            &mut resource_waiting,
                        );
                        if !planned.instance.rule.allow_failure && !pipeline_failed {
                            pipeline_failed = true;
                            halt_kind = HaltKind::JobFailure;
                            if halt_error.is_none() {
                                halt_error =
                                    Some(anyhow!("job '{}' failed: {}", event.name, err_msg));
                            }
                            while let Some(name) = waiting_on_failure.pop_front() {
                                ready.push_back(name);
                            }
                        }
                        summaries.push(JobSummary {
                            name: event.name.clone(),
                            stage_name: event.stage_name.clone(),
                            duration: event.duration,
                            status: JobStatus::Failed(err_msg),
                            log_path: event.log_path.clone(),
                            log_hash: event.log_hash.clone(),
                            allow_failure: planned.instance.rule.allow_failure,
                            environment: exec.expanded_environment(&planned.instance.job),
                        });
                        completed += 1;
                    }
                }
            }
        }
    }

    let skip_reason = match halt_kind {
        HaltKind::JobFailure => Some("not run (pipeline stopped after failure)".to_string()),
        HaltKind::Deadlock => Some("not run (dependency cycle detected)".to_string()),
        HaltKind::ChannelClosed => {
            Some("not run (executor channel closed unexpectedly)".to_string())
        }
        HaltKind::Aborted => Some("not run (pipeline aborted by user)".to_string()),
        HaltKind::None => None,
    };

    let mut recorded: HashSet<String> = summaries.iter().map(|entry| entry.name.clone()).collect();
    for job_name in &plan.ordered {
        if recorded.contains(job_name) {
            continue;
        }
        let Some(planned) = plan.nodes.get(job_name) else {
            continue;
        };
        let reason = if let Some(reason) = skip_reason.clone() {
            Some(reason)
        } else if planned.instance.rule.when == RuleWhen::OnFailure {
            Some("skipped (rules: on_failure and pipeline succeeded)".to_string())
        } else {
            None
        };

        if let Some(reason) = reason {
            if let Some(ui_ref) = ui.as_deref() {
                ui_ref.job_finished(job_name, UiJobStatus::Skipped, 0.0, Some(reason.clone()));
            }
            summaries.push(JobSummary {
                name: job_name.clone(),
                stage_name: planned.instance.stage_name.clone(),
                duration: 0.0,
                status: JobStatus::Skipped(reason.clone()),
                log_path: Some(planned.log_path.clone()),
                log_hash: planned.log_hash.clone(),
                allow_failure: planned.instance.rule.allow_failure,
                environment: exec.expanded_environment(&planned.instance.job),
            });
            recorded.insert(job_name.clone());
        }
    }

    let result = halt_error.map_or(Ok(()), Err);
    (summaries, result)
}

fn retry_allowed(
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

pub(crate) async fn handle_restart_commands(
    exec: &ExecutorCore,
    plan: Arc<ExecutionPlan>,
    ui: Option<Arc<UiBridge>>,
    commands: &mut mpsc::UnboundedReceiver<UiCommand>,
    summaries: &mut Vec<JobSummary>,
) -> Result<()> {
    while let Some(command) = commands.recv().await {
        match command {
            UiCommand::RestartJob { name } => {
                let Some(planned) = plan.nodes.get(&name).cloned() else {
                    continue;
                };

                if let Some(ui_ref) = ui.as_deref() {
                    ui_ref.job_restarted(&name);
                }

                let run_info = match exec.log_job_start(&planned, ui.as_deref()) {
                    Ok(info) => info,
                    Err(err) => {
                        summaries.push(JobSummary {
                            name: planned.instance.job.name.clone(),
                            stage_name: planned.instance.stage_name.clone(),
                            duration: 0.0,
                            status: JobStatus::Failed(err.to_string()),
                            log_path: Some(planned.log_path.clone()),
                            log_hash: planned.log_hash.clone(),
                            allow_failure: false,
                            environment: exec.expanded_environment(&planned.instance.job),
                        });
                        return Err(err);
                    }
                };
                let restart_exec = exec.clone();
                let ui_clone = ui.clone();
                let run_info_clone = run_info.clone();
                let job_plan = plan.clone();
                let event = task::spawn_blocking(move || {
                    job_runner::run_planned_job(
                        &restart_exec,
                        job_plan,
                        planned,
                        run_info_clone,
                        ui_clone,
                    )
                })
                .await
                .context("job restart task failed")?;
                update_summaries_from_event(exec, plan.as_ref(), event, summaries);
            }
            UiCommand::StartManual { .. } => {}
            UiCommand::CancelJob { .. } => {}
            UiCommand::AbortPipeline => break,
        }
    }
    Ok(())
}

fn update_summaries_from_event(
    exec: &ExecutorCore,
    plan: &ExecutionPlan,
    event: JobEvent,
    summaries: &mut Vec<JobSummary>,
) {
    let JobEvent {
        name,
        stage_name,
        duration,
        log_path,
        log_hash,
        result,
        failure_kind: _,
        exit_code: _,
        cancelled,
    } = event;

    let allow_failure = plan
        .nodes
        .get(&name)
        .map(|planned| planned.instance.rule.allow_failure)
        .unwrap_or(false);
    let environment = plan
        .nodes
        .get(&name)
        .and_then(|planned| exec.expanded_environment(&planned.instance.job));

    let status = match result {
        Ok(_) => JobStatus::Success,
        Err(err) => {
            if cancelled {
                JobStatus::Skipped("aborted by user".to_string())
            } else {
                JobStatus::Failed(err.to_string())
            }
        }
    };
    let outcome = match &status {
        JobStatus::Success => ArtifactSourceOutcome::Success,
        JobStatus::Failed(_) => ArtifactSourceOutcome::Failed,
        JobStatus::Skipped(_) => ArtifactSourceOutcome::Skipped,
    };
    exec.record_completed_job(&name, outcome);

    summaries.retain(|entry| entry.name != name);
    summaries.push(JobSummary {
        name,
        stage_name,
        duration,
        status,
        log_path,
        log_hash,
        allow_failure,
        environment,
    });
}

struct ReadyQueues<'a> {
    ready: &'a mut VecDeque<String>,
    waiting_on_failure: &'a mut VecDeque<String>,
    delayed_pending: &'a mut HashSet<String>,
}

fn release_dependents<F>(
    plan: &ExecutionPlan,
    name: &str,
    remaining: &mut HashMap<String, usize>,
    abort_requested: bool,
    pipeline_failed: bool,
    queues: &mut ReadyQueues<'_>,
    enqueue_ready: &F,
) where
    F: Fn(&str, bool, &mut VecDeque<String>, &mut VecDeque<String>, &mut HashSet<String>),
{
    if let Some(children) = plan.dependents.get(name) {
        for child in children {
            if let Some(count) = remaining.get_mut(child)
                && *count > 0
            {
                *count -= 1;
                if *count == 0 && !abort_requested {
                    enqueue_ready(
                        child,
                        pipeline_failed,
                        queues.ready,
                        queues.waiting_on_failure,
                        queues.delayed_pending,
                    );
                }
            }
        }
    }
}

fn release_resource_lock(
    planned: &ExecutableJob,
    ready: &mut VecDeque<String>,
    resource_locks: &mut HashMap<String, bool>,
    resource_waiting: &mut HashMap<String, VecDeque<String>>,
) {
    if let Some(group) = &planned.instance.resource_group {
        resource_locks.insert(group.clone(), false);
        if let Some(queue) = resource_waiting.get_mut(group)
            && let Some(next) = queue.pop_front()
        {
            ready.push_back(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{release_resource_lock, retry_allowed};
    use crate::compiler::JobInstance;
    use crate::execution_plan::ExecutableJob;
    use crate::model::{ArtifactSpec, JobSpec, RetryPolicySpec};
    use crate::pipeline::{JobFailureKind, RuleEvaluation, RuleWhen};
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;

    #[test]
    fn release_resource_lock_requeues_next_waiting_job() {
        let planned = ExecutableJob {
            instance: JobInstance {
                job: job("build"),
                stage_name: "build".into(),
                dependencies: Vec::new(),
                rule: RuleEvaluation {
                    included: true,
                    when: RuleWhen::OnSuccess,
                    ..Default::default()
                },
                timeout: None,
                retry: RetryPolicySpec::default(),
                interruptible: false,
                resource_group: Some("builder".into()),
            },
            log_path: PathBuf::from("/tmp/build.log"),
            log_hash: "hash".into(),
        };
        let mut ready = VecDeque::new();
        let mut resource_locks = HashMap::from([("builder".to_string(), true)]);
        let mut resource_waiting = HashMap::from([(
            "builder".to_string(),
            VecDeque::from(["package".to_string()]),
        )]);

        release_resource_lock(
            &planned,
            &mut ready,
            &mut resource_locks,
            &mut resource_waiting,
        );

        assert_eq!(ready, VecDeque::from(["package".to_string()]));
        assert_eq!(resource_locks.get("builder"), Some(&false));
        assert!(resource_waiting["builder"].is_empty());
    }

    #[test]
    fn retry_allowed_defaults_to_true_without_conditions() {
        assert!(retry_allowed(
            &[],
            &[],
            Some(JobFailureKind::ScriptFailure),
            Some(1)
        ));
    }

    #[test]
    fn retry_allowed_matches_script_failure_condition() {
        assert!(retry_allowed(
            &["script_failure".into()],
            &[],
            Some(JobFailureKind::ScriptFailure),
            Some(1)
        ));
        assert!(!retry_allowed(
            &["runner_system_failure".into()],
            &[],
            Some(JobFailureKind::ScriptFailure),
            Some(1)
        ));
    }

    #[test]
    fn retry_allowed_treats_job_timeout_as_stuck_or_timeout_failure() {
        assert!(retry_allowed(
            &["stuck_or_timeout_failure".into()],
            &[],
            Some(JobFailureKind::JobExecutionTimeout),
            None
        ));
    }

    #[test]
    fn retry_allowed_matches_api_failure_condition() {
        assert!(retry_allowed(
            &["api_failure".into()],
            &[],
            Some(JobFailureKind::ApiFailure),
            None
        ));
        assert!(!retry_allowed(
            &["api_failure".into()],
            &[],
            Some(JobFailureKind::UnknownFailure),
            None
        ));
    }

    #[test]
    fn retry_allowed_matches_unmet_prerequisites_condition() {
        assert!(retry_allowed(
            &["unmet_prerequisites".into()],
            &[],
            Some(JobFailureKind::UnmetPrerequisites),
            None
        ));
    }

    #[test]
    fn retry_allowed_matches_exit_code_condition() {
        assert!(retry_allowed(
            &[],
            &[137],
            Some(JobFailureKind::ScriptFailure),
            Some(137)
        ));
        assert!(!retry_allowed(
            &[],
            &[137],
            Some(JobFailureKind::ScriptFailure),
            Some(1)
        ));
    }

    #[test]
    fn retry_allowed_matches_when_or_exit_code() {
        assert!(retry_allowed(
            &["runner_system_failure".into()],
            &[137],
            Some(JobFailureKind::ScriptFailure),
            Some(137)
        ));
    }

    fn job(name: &str) -> JobSpec {
        JobSpec {
            name: name.into(),
            stage: "build".into(),
            commands: vec!["true".into()],
            needs: Vec::new(),
            explicit_needs: false,
            dependencies: Vec::new(),
            before_script: None,
            after_script: None,
            inherit_default_before_script: true,
            inherit_default_after_script: true,
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
