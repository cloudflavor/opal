use super::manual::{ManualJobAction, ManualJobs, manual_skip_reason};
use super::resource_groups::{ResourceAcquire, ResourceGroups};
use super::retry::retry_allowed;
use super::{planned_summary, running_jobs_in_plan_order, spawn_analysis, spawn_prompt_preview};
use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::executor::core::{
    ExecutionProgressCallback, ExecutionProgressEvent, ExecutorCore, ProgressJobStatus,
};
use crate::model::ArtifactSourceOutcome;
use crate::pipeline::{self, HaltKind, JobEvent, JobStatus, JobSummary, RuleWhen};
use crate::runtime;
use crate::ui::{UiBridge, UiCommand, UiJobStatus};
use anyhow::{Result, anyhow};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::{
    sync::{Semaphore, mpsc},
    time as tokio_time,
};

pub(super) struct ExecutionCoordinator<'a> {
    exec: Arc<ExecutorCore>,
    plan: Arc<ExecutionPlan>,
    ui: Option<Arc<UiBridge>>,
    commands: Option<&'a mut mpsc::UnboundedReceiver<UiCommand>>,
    semaphore: Arc<Semaphore>,
    tx: mpsc::UnboundedSender<JobEvent>,
    rx: mpsc::UnboundedReceiver<JobEvent>,
    delay_tx: mpsc::UnboundedSender<String>,
    delay_rx: mpsc::UnboundedReceiver<String>,
    total: usize,
    remaining: HashMap<String, usize>,
    ready: VecDeque<String>,
    waiting_on_failure: VecDeque<String>,
    delayed_pending: HashSet<String>,
    running: HashSet<String>,
    manual_jobs: ManualJobs,
    resource_groups: ResourceGroups,
    attempts: HashMap<String, u32>,
    completed: usize,
    abort_requested: bool,
    pipeline_failed: bool,
    halt_kind: HaltKind,
    halt_error: Option<anyhow::Error>,
    summaries: Vec<JobSummary>,
    progress: Option<ExecutionProgressCallback>,
}

enum SchedulerEvent {
    Job(JobEvent),
    Delay(String),
    Command(UiCommand),
}

impl<'a> ExecutionCoordinator<'a> {
    pub(super) fn new(
        exec: &ExecutorCore,
        plan: Arc<ExecutionPlan>,
        ui: Option<Arc<UiBridge>>,
        commands: Option<&'a mut mpsc::UnboundedReceiver<UiCommand>>,
        progress: Option<ExecutionProgressCallback>,
    ) -> Self {
        let manual_input_available = commands.is_some();
        let total = plan.ordered.len();
        let remaining = plan
            .nodes
            .iter()
            .map(|(name, job)| (name.clone(), job.instance.dependencies.len()))
            .collect();
        let semaphore = Arc::new(Semaphore::new(exec.config.max_parallel_jobs.max(1)));
        let (tx, rx) = mpsc::unbounded_channel();
        let (delay_tx, delay_rx) = mpsc::unbounded_channel();

        Self {
            exec: Arc::new(exec.clone()),
            plan,
            ui,
            commands,
            semaphore,
            tx,
            rx,
            delay_tx,
            delay_rx,
            total,
            remaining,
            ready: VecDeque::new(),
            waiting_on_failure: VecDeque::new(),
            delayed_pending: HashSet::new(),
            running: HashSet::new(),
            manual_jobs: ManualJobs::new(manual_input_available),
            resource_groups: ResourceGroups::new(runtime::resource_group_root()),
            attempts: HashMap::new(),
            completed: 0,
            abort_requested: false,
            pipeline_failed: false,
            halt_kind: HaltKind::None,
            halt_error: None,
            summaries: Vec::new(),
            progress,
        }
    }

    pub(super) async fn run(mut self) -> (Vec<JobSummary>, Result<()>) {
        if self.total == 0 {
            return (Vec::new(), Ok(()));
        }

        self.seed_initial_ready().await;

        while self.completed < self.total {
            if let Err(err) = self.schedule_ready_jobs().await {
                return (self.summaries, Err(err));
            }
            if self.completed >= self.total || self.should_stop() {
                break;
            }

            let Some(event) = self.next_event().await else {
                if self.running.is_empty()
                    && self.ready.is_empty()
                    && self.delayed_pending.is_empty()
                    && self.resource_groups.retry_pending_is_empty()
                {
                    self.halt_kind = HaltKind::ChannelClosed;
                    self.halt_error = Some(anyhow!(
                        "job worker channel closed unexpectedly while {} jobs remained",
                        self.total - self.completed
                    ));
                    break;
                }
                continue;
            };

            self.handle_event(event).await;
        }

        self.finish_unrecorded_jobs().await;
        let result = self.halt_error.map_or(Ok(()), Err);
        (self.summaries, result)
    }

    async fn schedule_ready_jobs(&mut self) -> Result<()> {
        while let Some(name) = self.ready.pop_front() {
            if self.abort_requested {
                break;
            }
            let Some(planned) = self.plan.nodes.get(&name).cloned() else {
                continue;
            };
            if self.pipeline_failed && planned.instance.rule.when.requires_success() {
                continue;
            }

            match self.manual_jobs.classify(&planned, self.ui.as_deref()) {
                ManualJobAction::NotManual => {}
                ManualJobAction::WaitForInput => continue,
                ManualJobAction::Skip { reason } => {
                    self.skip_job(&planned, reason, None).await;
                    self.release_dependents(&name).await;
                    continue;
                }
            }

            let scheduler_idle = self.running.is_empty() && self.ready.is_empty();
            match self
                .resource_groups
                .acquire_for_job(&planned, scheduler_idle, &self.delay_tx)
                .await
            {
                ResourceAcquire::Acquired => {}
                ResourceAcquire::RetryScheduled => continue,
                ResourceAcquire::Failed(err) => {
                    self.fail_pipeline(HaltKind::JobFailure, err);
                    break;
                }
            }

            self.start_job(planned).await?;
        }

        Ok(())
    }

    async fn next_event(&mut self) -> Option<SchedulerEvent> {
        tokio::select! {
            Some(event) = self.rx.recv() => Some(SchedulerEvent::Job(event)),
            Some(name) = self.delay_rx.recv() => Some(SchedulerEvent::Delay(name)),
            cmd = async {
                if let Some(rx) = self.commands.as_mut() {
                    (*rx).recv().await
                } else {
                    std::future::pending().await
                }
            } => {
                match cmd {
                    Some(command) => Some(SchedulerEvent::Command(command)),
                    None => {
                        self.commands = None;
                        self.skip_closed_manual_jobs().await;
                        None
                    }
                }
            }
            else => None,
        }
    }

    async fn handle_event(&mut self, event: SchedulerEvent) {
        match event {
            SchedulerEvent::Delay(name) => self.handle_delay(name).await,
            SchedulerEvent::Command(command) => self.handle_command(command).await,
            SchedulerEvent::Job(event) => self.handle_job_event(event).await,
        }
    }

    async fn seed_initial_ready(&mut self) {
        for name in self.plan.ordered.clone() {
            if self.remaining.get(&name).copied().unwrap_or(0) == 0 && !self.abort_requested {
                self.enqueue_ready(&name).await;
            }
        }
    }

    async fn enqueue_ready(&mut self, job_name: &str) {
        let Some(planned) = self.plan.nodes.get(job_name) else {
            return;
        };
        match planned.instance.rule.when {
            RuleWhen::OnFailure => {
                if self.pipeline_failed {
                    self.ready.push_back(job_name.to_string());
                } else {
                    self.waiting_on_failure.push_back(job_name.to_string());
                }
            }
            RuleWhen::Delayed => {
                if self.pipeline_failed {
                    return;
                }
                if let Some(delay) = planned.instance.rule.start_in {
                    if self.delayed_pending.insert(job_name.to_string()) {
                        let tx_clone = self.delay_tx.clone();
                        let name = job_name.to_string();
                        tokio::spawn(async move {
                            tokio_time::sleep(delay).await;
                            let _ = tx_clone.send(name);
                        });
                    }
                } else {
                    self.ready.push_back(job_name.to_string());
                }
            }
            RuleWhen::Manual | RuleWhen::OnSuccess => {
                if self.pipeline_failed && planned.instance.rule.when.requires_success() {
                    return;
                }
                self.ready.push_back(job_name.to_string());
            }
            RuleWhen::Always => self.ready.push_back(job_name.to_string()),
            RuleWhen::Never => {}
        }
    }

    async fn release_dependents(&mut self, name: &str) {
        if let Some(children) = self.plan.dependents.get(name).cloned() {
            for child in children {
                if let Some(count) = self.remaining.get_mut(&child)
                    && *count > 0
                {
                    *count -= 1;
                    if *count == 0 && !self.abort_requested {
                        self.enqueue_ready(&child).await;
                    }
                }
            }
        }
    }

    async fn start_job(&mut self, planned: ExecutableJob) -> Result<()> {
        let name = planned.instance.job.name.clone();
        let stage = planned.instance.stage_name.clone();
        *self.attempts.entry(name.clone()).or_insert(0) += 1;

        let run_info = self.exec.log_job_start(&planned, self.ui.as_deref())?;
        self.running.insert(name.clone());
        if let Some(progress) = self.progress.as_ref() {
            progress(ExecutionProgressEvent::JobStarted { name, stage });
        }
        pipeline::spawn_job(
            self.exec.clone(),
            self.plan.clone(),
            planned,
            run_info,
            self.semaphore.clone(),
            self.tx.clone(),
            self.ui.clone(),
        );
        Ok(())
    }

    async fn handle_delay(&mut self, name: String) {
        if self.abort_requested {
            return;
        }
        if self.resource_groups.consume_retry(&name) {
            self.ready.push_back(name);
            return;
        }
        self.delayed_pending.remove(&name);
        if self.pipeline_failed
            && let Some(planned) = self.plan.nodes.get(&name)
            && planned.instance.rule.when.requires_success()
        {
            return;
        }
        self.ready.push_back(name);
    }

    async fn handle_command(&mut self, command: UiCommand) {
        match command {
            UiCommand::StartManual { name } => {
                if self.manual_jobs.start(&name) {
                    self.ready.push_back(name);
                }
            }
            UiCommand::CancelJob { name } => {
                self.exec.cancel_running_job(&name);
            }
            UiCommand::AnalyzeJob { name, source_name } => {
                spawn_analysis(
                    self.exec.clone(),
                    self.plan.clone(),
                    self.ui.clone(),
                    name,
                    source_name,
                );
            }
            UiCommand::PreviewAiPrompt { name, source_name } => {
                spawn_prompt_preview(
                    self.exec.clone(),
                    self.plan.clone(),
                    self.ui.clone(),
                    name,
                    source_name,
                );
            }
            UiCommand::AbortPipeline => {
                self.abort_requested = true;
                self.fail_pipeline(HaltKind::Aborted, anyhow!("pipeline aborted by user"));
                for name in running_jobs_in_plan_order(self.plan.as_ref(), &self.running) {
                    self.exec.cancel_running_job(&name);
                }
                self.ready.clear();
                self.waiting_on_failure.clear();
                self.delayed_pending.clear();
                self.manual_jobs.clear();
            }
            UiCommand::RestartJob { .. } => {}
        }
    }

    async fn handle_job_event(&mut self, event: JobEvent) {
        self.running.remove(&event.name);

        let Some(planned) = self.plan.nodes.get(&event.name).cloned() else {
            let message = format!(
                "completed job '{}' was not found in execution plan",
                event.name
            );
            if !self.pipeline_failed {
                self.fail_pipeline(HaltKind::JobFailure, anyhow!(message.clone()));
            }
            self.summaries.push(JobSummary {
                name: event.name,
                stage_name: event.stage_name,
                duration: event.duration,
                status: JobStatus::Failed(message),
                log_path: event.log_path,
                log_hash: event.log_hash,
                allow_failure: false,
                environment: None,
            });
            self.completed += 1;
            return;
        };

        if event.cancelled {
            self.handle_cancelled(&planned, event).await;
            return;
        }

        if event.result.is_ok() {
            self.handle_success(&planned, event).await;
            return;
        }

        let err_msg = event
            .result
            .as_ref()
            .err()
            .map(|err| err.to_string())
            .unwrap_or_else(|| "job failed".to_string());
        self.handle_failure(&planned, event, err_msg).await;
    }

    async fn handle_success(&mut self, planned: &ExecutableJob, event: JobEvent) {
        if let Some(progress) = self.progress.as_ref() {
            progress(ExecutionProgressEvent::JobFinished {
                name: event.name.clone(),
                stage: event.stage_name.clone(),
                status: ProgressJobStatus::Success,
            });
        }
        self.exec
            .record_completed_job(&event.name, ArtifactSourceOutcome::Success);
        self.resource_groups.release(planned).await;
        self.release_dependents(&event.name).await;
        self.summaries.push(planned_summary(
            self.exec.as_ref(),
            planned,
            event.duration,
            JobStatus::Success,
            event.log_path,
            planned.instance.rule.allow_failure,
        ));
        self.completed += 1;
    }

    async fn handle_cancelled(&mut self, planned: &ExecutableJob, event: JobEvent) {
        if let Some(progress) = self.progress.as_ref() {
            progress(ExecutionProgressEvent::JobFinished {
                name: event.name.clone(),
                stage: event.stage_name.clone(),
                status: ProgressJobStatus::Skipped,
            });
        }
        self.exec
            .record_completed_job(&event.name, ArtifactSourceOutcome::Skipped);
        self.resource_groups.release(planned).await;
        self.summaries.push(planned_summary(
            self.exec.as_ref(),
            planned,
            event.duration,
            JobStatus::Skipped("aborted by user".to_string()),
            event.log_path,
            true,
        ));
        self.completed += 1;
    }

    async fn handle_failure(&mut self, planned: &ExecutableJob, event: JobEvent, err_msg: String) {
        let retries_used = self
            .attempts
            .get(&event.name)
            .copied()
            .unwrap_or(1)
            .saturating_sub(1);
        if retries_used < planned.instance.retry.max
            && retry_allowed(
                &planned.instance.retry.when,
                &planned.instance.retry.exit_codes,
                event.failure_kind,
                event.exit_code,
            )
        {
            self.resource_groups.release(planned).await;
            self.ready.push_back(event.name);
            return;
        }

        if let Some(progress) = self.progress.as_ref() {
            progress(ExecutionProgressEvent::JobFinished {
                name: event.name.clone(),
                stage: event.stage_name.clone(),
                status: ProgressJobStatus::Failed,
            });
        }
        self.exec
            .record_completed_job(&event.name, ArtifactSourceOutcome::Failed);
        self.resource_groups.release(planned).await;
        if !planned.instance.rule.allow_failure && !self.pipeline_failed {
            self.fail_pipeline(
                HaltKind::JobFailure,
                anyhow!("job '{}' failed: {}", event.name, err_msg),
            );
            self.release_on_failure_jobs();
        }
        self.release_dependents(&event.name).await;
        self.summaries.push(planned_summary(
            self.exec.as_ref(),
            planned,
            event.duration,
            JobStatus::Failed(err_msg),
            event.log_path,
            planned.instance.rule.allow_failure,
        ));
        self.completed += 1;
    }

    fn release_on_failure_jobs(&mut self) {
        while let Some(name) = self.waiting_on_failure.pop_front() {
            self.ready.push_back(name);
        }
    }

    async fn skip_job(
        &mut self,
        planned: &ExecutableJob,
        reason: String,
        log_path: Option<std::path::PathBuf>,
    ) {
        if let Some(progress) = self.progress.as_ref() {
            progress(ExecutionProgressEvent::JobFinished {
                name: planned.instance.job.name.clone(),
                stage: planned.instance.stage_name.clone(),
                status: ProgressJobStatus::Skipped,
            });
        }
        if let Some(ui_ref) = self.ui.as_deref() {
            ui_ref.job_finished(
                &planned.instance.job.name,
                UiJobStatus::Skipped,
                0.0,
                Some(reason.clone()),
            );
        }
        self.summaries.push(planned_summary(
            self.exec.as_ref(),
            planned,
            0.0,
            JobStatus::Skipped(reason),
            log_path,
            planned.instance.rule.allow_failure,
        ));
        self.completed += 1;
    }

    async fn skip_closed_manual_jobs(&mut self) {
        for name in self.manual_jobs.close_input() {
            if let Some(planned) = self.plan.nodes.get(&name).cloned() {
                self.skip_job(&planned, manual_skip_reason(&planned), None)
                    .await;
                self.release_dependents(&name).await;
            }
        }
    }

    async fn finish_unrecorded_jobs(&mut self) {
        let skip_reason = match self.halt_kind {
            HaltKind::JobFailure => Some("not run (pipeline stopped after failure)".to_string()),
            HaltKind::Deadlock => Some("not run (dependency cycle detected)".to_string()),
            HaltKind::ChannelClosed => {
                Some("not run (executor channel closed unexpectedly)".to_string())
            }
            HaltKind::Aborted => Some("not run (pipeline aborted by user)".to_string()),
            HaltKind::None => None,
        };

        let mut recorded: HashSet<String> = self
            .summaries
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        for job_name in &self.plan.ordered {
            if recorded.contains(job_name) {
                continue;
            }
            let Some(planned) = self.plan.nodes.get(job_name) else {
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
                if let Some(ui_ref) = self.ui.as_deref() {
                    ui_ref.job_finished(job_name, UiJobStatus::Skipped, 0.0, Some(reason.clone()));
                }
                self.summaries.push(planned_summary(
                    self.exec.as_ref(),
                    planned,
                    0.0,
                    JobStatus::Skipped(reason.clone()),
                    Some(planned.log_path.clone()),
                    planned.instance.rule.allow_failure,
                ));
                recorded.insert(job_name.clone());
            }
        }
    }

    fn should_stop(&mut self) -> bool {
        if self.running.is_empty()
            && self.ready.is_empty()
            && self.delayed_pending.is_empty()
            && self.resource_groups.retry_pending_is_empty()
            && self.pipeline_failed
            && self.waiting_on_failure.is_empty()
            && self.manual_jobs.is_empty()
        {
            return true;
        }

        if self.running.is_empty()
            && self.ready.is_empty()
            && self.delayed_pending.is_empty()
            && self.resource_groups.retry_pending_is_empty()
            && !self.pipeline_failed
            && self.waiting_on_failure.is_empty()
            && self.manual_jobs.is_empty()
        {
            let remaining_jobs: Vec<_> = self
                .remaining
                .iter()
                .filter_map(|(name, &count)| if count > 0 { Some(name.clone()) } else { None })
                .collect();
            if !remaining_jobs.is_empty() {
                self.halt_kind = HaltKind::Deadlock;
                self.halt_error = Some(anyhow!(
                    "no runnable jobs, potential dependency cycle involving: {:?}",
                    remaining_jobs
                ));
            }
            return true;
        }

        self.running.is_empty()
            && self.ready.is_empty()
            && self.delayed_pending.is_empty()
            && self.resource_groups.retry_pending_is_empty()
            && !self.pipeline_failed
            && !self.waiting_on_failure.is_empty()
            && self.manual_jobs.is_empty()
    }

    fn fail_pipeline(&mut self, kind: HaltKind, err: anyhow::Error) {
        self.pipeline_failed = true;
        self.halt_kind = kind;
        if self.halt_error.is_none() {
            self.halt_error = Some(err);
        }
    }
}
