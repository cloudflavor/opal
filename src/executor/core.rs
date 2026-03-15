use super::{
    ContainerExecutor, DockerExecutor, NerdctlExecutor, OrbstackExecutor, PodmanExecutor, paths,
    script, services::ServiceRuntime,
};
use crate::display::{
    self, DisplayFormatter, collect_pipeline_plan, indent_block, print_pipeline_summary,
};
use crate::engine::EngineCommandContext;
use crate::env::{build_job_env, collect_env_vars, expand_env_list};
use crate::gitlab::{CachePolicy, Job, PipelineGraph, ServiceConfig};
use crate::history::{self, HistoryCache, HistoryEntry, HistoryJob, HistoryStatus};
use crate::logging::{self, LogFormatter, sanitize_fragments};
use crate::naming::{generate_run_id, job_name_slug, stage_name_slug};
use crate::pipeline::{
    self, ArtifactManager, CacheManager, ExternalArtifactsManager, HaltKind, JobEvent, JobPlan,
    JobRunInfo, JobStatus, JobSummary, PlannedJob, RuleContext, RuleWhen, StageState, VolumeMount,
    mounts,
};
use crate::runner::ExecuteContext;
use crate::secrets::SecretsStore;
use crate::terminal::{should_use_color, stream_lines};
use crate::ui::{UiBridge, UiCommand, UiHandle, UiJobInfo, UiJobResources, UiJobStatus};
use crate::{EngineKind, ExecutorConfig};
use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};
use tokio::{
    sync::{Semaphore, mpsc},
    task, time as tokio_time,
};
use tracing::warn;

pub(super) const CONTAINER_WORKDIR: &str = "/workspace";
const TIMESTAMP_FORMAT: &[FormatItem<'static>] =
    format_description!("[hour]:[minute]:[second].[subsecond digits:3]");

#[derive(Debug, Clone)]
pub struct ExecutorCore {
    pub config: ExecutorConfig,
    g: PipelineGraph,
    use_color: bool,
    scripts_dir: PathBuf,
    logs_dir: PathBuf,
    session_dir: PathBuf,
    run_id: String,
    verbose_scripts: bool,
    env_vars: Vec<(String, String)>,
    host_env: HashMap<String, String>,
    stage_positions: HashMap<String, usize>,
    stage_states: Arc<Mutex<HashMap<String, StageState>>>,
    job_attempts: Arc<Mutex<HashMap<String, usize>>>,
    history_path: PathBuf,
    history_entries: Arc<Mutex<Vec<HistoryEntry>>>,
    secrets: SecretsStore,
    artifacts: ArtifactManager,
    cache: CacheManager,
    external_artifacts: Option<ExternalArtifactsManager>,
    running_containers: Arc<Mutex<HashMap<String, String>>>,
    cancelled_jobs: Arc<Mutex<HashSet<String>>>,
}

#[derive(Debug, Clone)]
struct JobResourceInfo {
    artifact_dir: Option<String>,
    artifact_paths: Vec<String>,
    caches: Vec<HistoryCache>,
}

impl ExecutorCore {
    pub fn new(config: ExecutorConfig) -> Result<Self> {
        let g = PipelineGraph::from_path(&config.pipeline)?;
        let run_id = generate_run_id(&config);
        let sessions_root = config.workdir.join(".opal");
        fs::create_dir_all(&sessions_root)
            .with_context(|| format!("failed to create {:?}", sessions_root))?;

        let session_dir = sessions_root.join(&run_id);
        if session_dir.exists() {
            fs::remove_dir_all(&session_dir)
                .with_context(|| format!("failed to clean {:?}", session_dir))?;
        }
        fs::create_dir_all(&session_dir)
            .with_context(|| format!("failed to create {:?}", session_dir))?;

        let scripts_dir = session_dir.join("scripts");
        fs::create_dir_all(&scripts_dir)
            .with_context(|| format!("failed to create {:?}", scripts_dir))?;

        let logs_root = config
            .log_dir
            .clone()
            .map(|dir| {
                if dir.is_absolute() {
                    dir
                } else {
                    config.workdir.join(dir)
                }
            })
            .unwrap_or_else(|| sessions_root.join("logs"));
        fs::create_dir_all(&logs_root)
            .with_context(|| format!("failed to create {:?}", logs_root))?;
        let logs_dir = logs_root.join(&run_id);
        if logs_dir.exists() {
            fs::remove_dir_all(&logs_dir)
                .with_context(|| format!("failed to clean {:?}", logs_dir))?;
        }
        fs::create_dir_all(&logs_dir)
            .with_context(|| format!("failed to create {:?}", logs_dir))?;

        let history_path = sessions_root.join("history.json");
        let history_entries = match history::load(&history_path) {
            Ok(entries) => entries,
            Err(err) => {
                warn!(
                    error = %err,
                    path = %history_path.display(),
                    "failed to load pipeline history"
                );
                Vec::new()
            }
        };

        let use_color = should_use_color();
        let verbose_scripts = env::var_os("OPAL_DEBUG")
            .map(|val| {
                let s = val.to_string_lossy();
                s == "1" || s.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false);
        let host_env: HashMap<String, String> = std::env::vars().collect();
        let mut env_vars = collect_env_vars(&config.env_includes)?;
        expand_env_list(&mut env_vars[..], &host_env);
        let mut stage_positions = HashMap::new();
        let mut stage_states = HashMap::new();
        for (idx, stage) in g.stages.iter().enumerate() {
            stage_positions.insert(stage.name.clone(), idx);
            stage_states.insert(stage.name.clone(), StageState::new(stage.jobs.len()));
        }

        let secrets = SecretsStore::load(&config.workdir)?;
        let artifacts = ArtifactManager::new(session_dir.clone());
        let cache_root = sessions_root.join("cache");
        fs::create_dir_all(&cache_root)
            .with_context(|| format!("failed to create cache root {:?}", cache_root))?;
        let cache = CacheManager::new(cache_root);
        let external_artifacts = config.gitlab.as_ref().map(|cfg| {
            ExternalArtifactsManager::new(
                session_dir.clone(),
                cfg.base_url.clone(),
                cfg.token.clone(),
            )
        });
        let running_containers = Arc::new(Mutex::new(HashMap::new()));
        let cancelled_jobs = Arc::new(Mutex::new(HashSet::new()));

        Ok(Self {
            config,
            g,
            use_color,
            scripts_dir,
            logs_dir,
            session_dir,
            run_id,
            verbose_scripts,
            env_vars,
            host_env,
            stage_positions,
            stage_states: Arc::new(Mutex::new(stage_states)),
            job_attempts: Arc::new(Mutex::new(HashMap::new())),
            history_path,
            history_entries: Arc::new(Mutex::new(history_entries)),
            secrets,
            artifacts,
            cache,
            external_artifacts,
            running_containers,
            cancelled_jobs,
        })
    }

    pub async fn run(&self) -> Result<()> {
        let plan = self.plan_jobs()?;
        let resource_map = self.collect_job_resources(&plan);
        let display = self.display();
        let plan_text = collect_pipeline_plan(&display, &plan).join("\n");
        let ui_resources = Self::convert_ui_resources(&resource_map);
        let history_snapshot = self
            .history_entries
            .lock()
            .map(|entries| entries.clone())
            .unwrap_or_default();
        let ui_handle = if self.config.enable_tui {
            let jobs: Vec<UiJobInfo> = plan
                .ordered
                .iter()
                .filter_map(|name| plan.nodes.get(name))
                .map(|planned| UiJobInfo {
                    name: planned.job.name.clone(),
                    stage: planned.stage_name.clone(),
                    log_path: planned.log_path.clone(),
                    log_hash: planned.log_hash.clone(),
                })
                .collect();
            Some(UiHandle::start(
                jobs,
                history_snapshot,
                self.run_id.clone(),
                ui_resources,
                plan_text,
                self.config.workdir.clone(),
            )?)
        } else {
            None
        };
        let mut command_rx = ui_handle
            .as_ref()
            .and_then(|handle| handle.command_receiver());
        let ui_bridge = ui_handle.as_ref().map(|handle| Arc::new(handle.bridge()));

        let (mut summaries, result) = self
            .execute_plan(&plan, ui_bridge.clone(), command_rx.as_mut())
            .await;

        if let Some(handle) = &ui_handle {
            handle.pipeline_finished();
        }

        if let Some(commands) = command_rx.as_mut() {
            self.handle_restart_commands(&plan, ui_bridge.clone(), commands, &mut summaries)
                .await?;
        }

        let history_entry = self.record_pipeline_history(&summaries, &resource_map);
        if let (Some(entry), Some(ui)) = (history_entry, ui_bridge.as_deref()) {
            ui.history_updated(entry);
        }

        if let Some(handle) = ui_handle {
            handle.wait_for_exit();
        }

        if !self.config.enable_tui {
            print_pipeline_summary(
                &display,
                &plan,
                &summaries,
                &self.session_dir,
                display::print_line,
            );
        }
        result
    }

    fn plan_jobs(&self) -> Result<JobPlan> {
        let ctx = RuleContext::new(&self.config.workdir);
        if !pipeline::rules::filters_allow(&self.g.filters, &ctx) {
            return Ok(JobPlan {
                ordered: Vec::new(),
                nodes: HashMap::new(),
                dependents: HashMap::new(),
                order_index: HashMap::new(),
            });
        }
        if let Some(workflow) = &self.g.workflow
            && !pipeline::rules::evaluate_workflow(&workflow.rules, &ctx)?
        {
            return Ok(JobPlan {
                ordered: Vec::new(),
                nodes: HashMap::new(),
                dependents: HashMap::new(),
                order_index: HashMap::new(),
            });
        }
        pipeline::build_job_plan(&self.g, Some(&ctx), |job| self.job_log_info(job))
    }

    fn collect_job_resources(&self, plan: &JobPlan) -> HashMap<String, JobResourceInfo> {
        plan.nodes
            .values()
            .map(|planned| {
                let artifact_dir = if planned.job.artifacts.is_empty() {
                    None
                } else {
                    Some(
                        self.artifacts
                            .job_artifacts_root(&planned.job.name)
                            .display()
                            .to_string(),
                    )
                };
                let artifact_paths = planned
                    .job
                    .artifacts
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect();
                let env_vars = self.job_env(&planned.job);
                let cache_env: HashMap<String, String> = env_vars.iter().cloned().collect();
                let caches = self
                    .cache
                    .describe_entries(&planned.job.cache, &cache_env)
                    .into_iter()
                    .map(|entry| HistoryCache {
                        key: entry.key,
                        policy: cache_policy_label(entry.policy).to_string(),
                        host: entry.host.display().to_string(),
                        paths: entry
                            .paths
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect(),
                    })
                    .collect();
                (
                    planned.job.name.clone(),
                    JobResourceInfo {
                        artifact_dir,
                        artifact_paths,
                        caches,
                    },
                )
            })
            .collect()
    }

    fn convert_ui_resources(
        resources: &HashMap<String, JobResourceInfo>,
    ) -> HashMap<String, UiJobResources> {
        resources
            .iter()
            .map(|(name, info)| {
                (
                    name.clone(),
                    UiJobResources {
                        artifact_dir: info.artifact_dir.clone(),
                        artifact_paths: info.artifact_paths.clone(),
                        caches: info.caches.clone(),
                    },
                )
            })
            .collect()
    }

    async fn execute_plan(
        &self,
        plan: &JobPlan,
        ui: Option<Arc<UiBridge>>,
        mut commands: Option<&mut mpsc::UnboundedReceiver<UiCommand>>,
    ) -> (Vec<JobSummary>, Result<()>) {
        let total = plan.ordered.len();
        if total == 0 {
            return (Vec::new(), Ok(()));
        }

        let mut remaining: HashMap<String, usize> = plan
            .nodes
            .iter()
            .map(|(name, job)| (name.clone(), job.dependencies.len()))
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

        let semaphore = Arc::new(Semaphore::new(self.config.max_parallel_jobs.max(1)));
        let exec = Arc::new(self.clone());
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
            match planned.rule.when {
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
                    if let Some(delay) = planned.rule.start_in {
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
                    if pipeline_failed_flag && planned.rule.when.requires_success() {
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
                if pipeline_failed && planned.rule.when.requires_success() {
                    continue;
                }

                if matches!(planned.rule.when, RuleWhen::Manual) && !planned.rule.manual_auto_run {
                    if manual_input_available {
                        if manual_waiting.insert(name.clone())
                            && let Some(ui_ref) = ui.as_deref()
                        {
                            ui_ref.job_manual_pending(&name);
                        }
                    } else {
                        let reason = planned
                            .rule
                            .manual_reason
                            .clone()
                            .unwrap_or_else(|| "manual job not run".to_string());
                        if let Some(ui_ref) = ui.as_deref() {
                            ui_ref.job_finished(
                                &planned.job.name,
                                UiJobStatus::Skipped,
                                0.0,
                                Some(reason.clone()),
                            );
                        }
                        summaries.push(JobSummary {
                            name: planned.job.name.clone(),
                            stage_name: planned.stage_name.clone(),
                            duration: 0.0,
                            status: JobStatus::Skipped(reason.clone()),
                            log_path: None,
                            log_hash: planned.log_hash.clone(),
                            allow_failure: planned.rule.allow_failure,
                            environment: planned.job.environment.clone(),
                        });
                        completed += 1;
                        if let Some(children) = plan.dependents.get(&name) {
                            for child in children {
                                if let Some(count) = remaining.get_mut(child)
                                    && *count > 0
                                {
                                    *count -= 1;
                                    if *count == 0 && !abort_requested {
                                        enqueue_ready(
                                            child,
                                            pipeline_failed,
                                            &mut ready,
                                            &mut waiting_on_failure,
                                            &mut delayed_pending,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }

                if let Some(group) = &planned.resource_group {
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

                let run_info = match self.log_job_start(&planned, ui.as_deref()) {
                    Ok(info) => info,
                    Err(err) => {
                        let summary = JobSummary {
                            name: planned.job.name.clone(),
                            stage_name: planned.stage_name.clone(),
                            duration: 0.0,
                            status: JobStatus::Failed(err.to_string()),
                            log_path: Some(planned.log_path.clone()),
                            log_hash: planned.log_hash.clone(),
                            allow_failure: false,
                            environment: planned.job.environment.clone(),
                        };
                        summaries.push(summary);
                        return (summaries, Err(err));
                    }
                };
                running.insert(name.clone());
                pipeline::spawn_job(
                    exec.clone(),
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
                            .rule
                            .manual_reason
                            .clone()
                            .unwrap_or_else(|| "manual job not run".to_string());
                        if let Some(ui_ref) = ui.as_deref() {
                            ui_ref.job_finished(
                                &planned.job.name,
                                UiJobStatus::Skipped,
                                0.0,
                                Some(reason.clone()),
                            );
                        }
                        summaries.push(JobSummary {
                            name: planned.job.name.clone(),
                            stage_name: planned.stage_name.clone(),
                            duration: 0.0,
                            status: JobStatus::Skipped(reason),
                            log_path: None,
                            log_hash: planned.log_hash.clone(),
                            allow_failure: planned.rule.allow_failure,
                            environment: planned.job.environment.clone(),
                        });
                        completed += 1;
                        if let Some(children) = plan.dependents.get(&name) {
                            for child in children {
                                if let Some(count) = remaining.get_mut(child)
                                    && *count > 0
                                {
                                    *count -= 1;
                                    if *count == 0 && !abort_requested {
                                        enqueue_ready(
                                            child,
                                            pipeline_failed,
                                            &mut ready,
                                            &mut waiting_on_failure,
                                            &mut delayed_pending,
                                        );
                                    }
                                }
                            }
                        }
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
                        && planned.rule.when.requires_success()
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
                        self.cancel_running_job(&name);
                    }
                    UiCommand::AbortPipeline => {
                        abort_requested = true;
                        pipeline_failed = true;
                        halt_kind = HaltKind::Aborted;
                        if halt_error.is_none() {
                            halt_error = Some(anyhow!("pipeline aborted by user"));
                        }
                        self.cancel_all_running_jobs();
                        ready.clear();
                        waiting_on_failure.clear();
                        delayed_pending.clear();
                        manual_waiting.clear();
                    }
                    UiCommand::RestartJob { .. } => {}
                },
                SchedulerEvent::Job(event) => {
                    running.remove(&event.name);
                    let planned = plan
                        .nodes
                        .get(&event.name)
                        .expect("completed job must exist in plan");
                    match event.result {
                        Ok(_) => {
                            release_resource_lock(
                                planned,
                                &mut ready,
                                &mut resource_locks,
                                &mut resource_waiting,
                            );
                            if let Some(children) = plan.dependents.get(&event.name) {
                                for child in children {
                                    if let Some(count) = remaining.get_mut(child)
                                        && *count > 0
                                    {
                                        *count -= 1;
                                        if *count == 0 && !abort_requested {
                                            enqueue_ready(
                                                child,
                                                pipeline_failed,
                                                &mut ready,
                                                &mut waiting_on_failure,
                                                &mut delayed_pending,
                                            );
                                        }
                                    }
                                }
                            }
                            summaries.push(JobSummary {
                                name: event.name.clone(),
                                stage_name: event.stage_name.clone(),
                                duration: event.duration,
                                status: JobStatus::Success,
                                log_path: event.log_path.clone(),
                                log_hash: event.log_hash.clone(),
                                allow_failure: planned.rule.allow_failure,
                                environment: planned.job.environment.clone(),
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
                                    environment: planned.job.environment.clone(),
                                });
                                completed += 1;
                                continue;
                            }
                            let err_msg = err.to_string();
                            let attempts_so_far = attempts.get(&event.name).copied().unwrap_or(1);
                            let retries_used = attempts_so_far.saturating_sub(1);
                            if retries_used < planned.retry.max {
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
                            if !planned.rule.allow_failure && !pipeline_failed {
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
                                allow_failure: planned.rule.allow_failure,
                                environment: planned.job.environment.clone(),
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

        let mut recorded: HashSet<String> =
            summaries.iter().map(|entry| entry.name.clone()).collect();
        for job_name in &plan.ordered {
            if recorded.contains(job_name) {
                continue;
            }
            let Some(planned) = plan.nodes.get(job_name) else {
                continue;
            };
            let reason = if let Some(reason) = skip_reason.clone() {
                Some(reason)
            } else if planned.rule.when == RuleWhen::OnFailure {
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
                    stage_name: planned.stage_name.clone(),
                    duration: 0.0,
                    status: JobStatus::Skipped(reason.clone()),
                    log_path: Some(planned.log_path.clone()),
                    log_hash: planned.log_hash.clone(),
                    allow_failure: planned.rule.allow_failure,
                    environment: planned.job.environment.clone(),
                });
                recorded.insert(job_name.clone());
            }
        }

        let result = halt_error.map_or(Ok(()), Err);
        (summaries, result)
    }

    async fn handle_restart_commands(
        &self,
        plan: &JobPlan,
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

                    let run_info = match self.log_job_start(&planned, ui.as_deref()) {
                        Ok(info) => info,
                        Err(err) => {
                            let summary = JobSummary {
                                name: planned.job.name.clone(),
                                stage_name: planned.stage_name.clone(),
                                duration: 0.0,
                                status: JobStatus::Failed(err.to_string()),
                                log_path: Some(planned.log_path.clone()),
                                log_hash: planned.log_hash.clone(),
                                allow_failure: false,
                                environment: planned.job.environment.clone(),
                            };
                            summaries.push(summary);
                            return Err(err);
                        }
                    };
                    let exec = self.clone();
                    let ui_clone = ui.clone();
                    let run_info_clone = run_info.clone();
                    let event = task::spawn_blocking(move || {
                        exec.run_planned_job(planned, run_info_clone, ui_clone)
                    })
                    .await
                    .context("job restart task failed")?;
                    self.update_summaries_from_event(plan, event, summaries);
                }
                UiCommand::StartManual { .. } => {}
                UiCommand::CancelJob { .. } => {}
                UiCommand::AbortPipeline => break,
            }
        }
        Ok(())
    }

    fn update_summaries_from_event(
        &self,
        plan: &JobPlan,
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
            cancelled,
        } = event;

        let allow_failure = plan
            .nodes
            .get(&name)
            .map(|planned| planned.rule.allow_failure)
            .unwrap_or(false);
        let environment = plan
            .nodes
            .get(&name)
            .and_then(|planned| planned.job.environment.clone());

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

    fn record_pipeline_history(
        &self,
        summaries: &[JobSummary],
        resources: &HashMap<String, JobResourceInfo>,
    ) -> Option<HistoryEntry> {
        let finished_at = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".to_string());
        let pipeline_status = if summaries
            .iter()
            .any(|entry| matches!(entry.status, JobStatus::Failed(_)) && !entry.allow_failure)
        {
            HistoryStatus::Failed
        } else if summaries
            .iter()
            .all(|entry| matches!(entry.status, JobStatus::Skipped(_)))
        {
            HistoryStatus::Skipped
        } else {
            HistoryStatus::Success
        };

        let jobs: Vec<HistoryJob> = summaries
            .iter()
            .map(|entry| HistoryJob {
                name: entry.name.clone(),
                stage: entry.stage_name.clone(),
                status: Self::history_status_for_job(&entry.status),
                log_hash: entry.log_hash.clone(),
                log_path: entry
                    .log_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                artifact_dir: resources
                    .get(&entry.name)
                    .and_then(|info| info.artifact_dir.clone()),
                artifacts: resources
                    .get(&entry.name)
                    .map(|info| info.artifact_paths.clone())
                    .unwrap_or_default(),
                caches: resources
                    .get(&entry.name)
                    .map(|info| info.caches.clone())
                    .unwrap_or_default(),
            })
            .collect();

        let entry = HistoryEntry {
            run_id: self.run_id.clone(),
            finished_at,
            status: pipeline_status,
            jobs,
        };

        match self.history_entries.lock() {
            Ok(mut existing) => {
                existing.push(entry.clone());
                if let Err(err) = history::save(&self.history_path, &existing) {
                    warn!(error = %err, "failed to persist pipeline history");
                }
                Some(entry)
            }
            Err(err) => {
                warn!(error = %err, "failed to record pipeline history");
                None
            }
        }
    }

    fn history_status_for_job(status: &JobStatus) -> HistoryStatus {
        match status {
            JobStatus::Success => HistoryStatus::Success,
            JobStatus::Failed(_) => HistoryStatus::Failed,
            JobStatus::Skipped(_) => HistoryStatus::Skipped,
        }
    }

    fn log_job_start(&self, planned: &PlannedJob, ui: Option<&UiBridge>) -> Result<JobRunInfo> {
        let attempt = self.next_attempt(&planned.job.name);
        let container_name = self.job_container_name(&planned.stage_name, &planned.job, attempt);
        if let Some(ui) = ui {
            ui.job_started(&planned.job.name);
        }

        if !self.config.enable_tui {
            let display = self.display();
            if self.stage_started(&planned.stage_name) {
                if self.stage_position(&planned.stage_name) > 0 {
                    display::print_blank_line();
                }
                display::print_line(display.stage_header(&planned.stage_name));
            }

            let job = &planned.job;
            let job_label = display.bold_green("  job:");
            let job_name = display.bold_white(job.name.as_str());
            display::print_line(format!("{} {}", job_label, job_name));

            if let Some(needs) = display.format_needs(job) {
                let needs_label = display.bold_cyan("    needs:");
                display::print_line(format!("{} {}", needs_label, needs));
            }
            if let Some(paths) = display.format_paths(&job.artifacts) {
                let artifacts_label = display.bold_cyan("    artifacts:");
                display::print_line(format!("{} {}", artifacts_label, paths));
            }

            let job_image = self.resolve_job_image(job)?;
            let image_label = display.bold_cyan("    image:");
            display::print_line(format!("{} {}", image_label, job_image));

            let container_label = display.bold_cyan("    container:");
            display::print_line(format!("{} {}", container_label, container_name));

            if self.verbose_scripts && !job.commands.is_empty() {
                let script_label = display.bold_yellow("    script:");
                display::print_line(format!(
                    "{}\n{}",
                    script_label,
                    indent_block(&job.commands.join("\n"), "      │ ")
                ));
            }
        }

        self.track_running_container(&planned.job.name, &container_name);
        Ok(JobRunInfo { container_name })
    }

    pub(crate) fn run_planned_job(
        &self,
        planned: PlannedJob,
        run_info: JobRunInfo,
        ui: Option<Arc<UiBridge>>,
    ) -> JobEvent {
        let PlannedJob {
            job,
            stage_name,
            log_path,
            log_hash,
            ..
        } = planned;
        let job_name = job.name.clone();
        let job_start = Instant::now();
        let ui_ref = ui.as_deref();

        let result = (|| -> Result<()> {
            self.artifacts.prepare_targets(&job)?;
            let env_vars = self.job_env(&job);
            let cache_env: HashMap<String, String> = env_vars.iter().cloned().collect();
            let service_configs = self.job_services(&job);
            let service_runtime = ServiceRuntime::start(
                self.config.engine,
                &self.run_id,
                &job.name,
                &service_configs,
                &env_vars,
                &self.host_env,
            )?;
            let service_network = service_runtime
                .as_ref()
                .map(|runtime| runtime.network_name().to_string());
            let mut mounts = mounts::collect_volume_mounts(
                &job,
                &self.g,
                &self.artifacts,
                &self.cache,
                &cache_env,
                Path::new(CONTAINER_WORKDIR),
                self.external_artifacts.as_ref(),
            )?;
            if let Some((host, container_path)) = self.secrets.volume_mount() {
                mounts.push(VolumeMount {
                    host,
                    container: container_path,
                    read_only: true,
                });
            }
            let job_image = self.resolve_job_image(&job)?;
            let container_name = run_info.container_name.clone();
            let script_commands = self.expanded_commands(&job);
            let script_path = script::write_job_script(
                &self.scripts_dir,
                &job,
                &script_commands,
                self.verbose_scripts,
            )?;
            let exec_result = self.execute(ExecuteContext {
                script_path: &script_path,
                log_path: &log_path,
                mounts: &mounts,
                image: &job_image,
                container_name: &container_name,
                job: &job,
                ui: ui_ref,
                env_vars: &env_vars,
                network: service_network.as_deref(),
            });
            if let Some(mut runtime) = service_runtime {
                runtime.cleanup();
            }
            exec_result?;
            if !self.config.enable_tui {
                let display = self.display();
                display::print_line(format!("    script stored at {}", script_path.display()));
                display::print_line(format!("    log file stored at {}", log_path.display()));
                let finish_label = display.bold_green("    ✓ finished in");
                display::print_line(format!(
                    "{} {:.2}s",
                    finish_label,
                    job_start.elapsed().as_secs_f32()
                ));

                if let Some(elapsed) = self.stage_job_completed(&stage_name) {
                    let stage_footer = display.bold_blue("╰─ stage complete in");
                    display::print_line(format!("{stage_footer} {:.2}s", elapsed));
                }
            }

            Ok(())
        })();

        let duration = job_start.elapsed().as_secs_f32();
        let cancelled = self.take_cancelled_job(&job_name);
        let final_result = if cancelled {
            Err(anyhow!("job cancelled by user"))
        } else {
            result
        };
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

        self.clear_running_container(&job_name);

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

    fn stage_started(&self, stage_name: &str) -> bool {
        let mut states = self
            .stage_states
            .lock()
            .expect("stage tracker mutex poisoned");
        let state = states
            .entry(stage_name.to_string())
            .or_insert_with(|| StageState::new(0));
        if state.header_printed {
            false
        } else {
            state.header_printed = true;
            state.started_at = Some(Instant::now());
            true
        }
    }

    fn stage_job_completed(&self, stage_name: &str) -> Option<f32> {
        let mut states = self
            .stage_states
            .lock()
            .expect("stage tracker mutex poisoned");
        let state = states.get_mut(stage_name)?;
        state.completed += 1;
        if state.completed == state.total {
            state.started_at.map(|start| start.elapsed().as_secs_f32())
        } else {
            None
        }
    }

    fn stage_position(&self, stage_name: &str) -> usize {
        self.stage_positions.get(stage_name).copied().unwrap_or(0)
    }

    fn track_running_container(&self, job_name: &str, container: &str) {
        if let Ok(mut map) = self.running_containers.lock() {
            map.insert(job_name.to_string(), container.to_string());
        }
    }

    fn clear_running_container(&self, job_name: &str) {
        if let Ok(mut map) = self.running_containers.lock() {
            map.remove(job_name);
        }
    }

    fn mark_job_cancelled(&self, job_name: &str) {
        if let Ok(mut cancelled) = self.cancelled_jobs.lock() {
            cancelled.insert(job_name.to_string());
        }
    }

    fn take_cancelled_job(&self, job_name: &str) -> bool {
        if let Ok(mut cancelled) = self.cancelled_jobs.lock() {
            cancelled.remove(job_name)
        } else {
            false
        }
    }

    fn cancel_running_job(&self, job_name: &str) -> bool {
        let container = {
            let map = match self.running_containers.lock() {
                Ok(map) => map,
                Err(_) => return false,
            };
            map.get(job_name).cloned()
        };
        if let Some(container_name) = container {
            self.mark_job_cancelled(job_name);
            self.kill_container(job_name, &container_name);
            true
        } else {
            false
        }
    }

    fn cancel_all_running_jobs(&self) {
        let containers: Vec<(String, String)> = match self.running_containers.lock() {
            Ok(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Err(_) => return,
        };
        for (job, container) in containers {
            self.mark_job_cancelled(&job);
            self.kill_container(&job, &container);
        }
    }

    fn execute(&self, ctx: ExecuteContext<'_>) -> Result<()> {
        let ExecuteContext {
            script_path,
            log_path,
            mounts,
            image,
            container_name,
            job,
            ui,
            env_vars,
            network,
        } = ctx;
        if !self.config.enable_tui {
            let display = self.display();
            display::print_line(display.format_mounts(mounts));
            display::print_line(display.logs_header());
            let log_label = display.bold_yellow("    log file:");
            display::print_line(format!("{} {}", log_label, log_path.display()));
        }

        let container_script = self.container_path_rel(script_path)?;
        if self.verbose_scripts && !self.config.enable_tui {
            let display = self.display();
            let script_label = display.bold_yellow("    script file:");
            display::print_line(format!("{} {}", script_label, container_script.display()));
        }

        let mut proc = self.spawn_container_process(
            &container_script,
            container_name,
            image,
            mounts,
            env_vars,
            network,
        )?;
        self.capture_child_output(&mut proc, job, log_path, ui)?;

        let status = proc.wait()?;
        if !status.success() {
            return Err(anyhow!(
                "container command exited with status {:?}",
                status.code()
            ));
        }

        Ok(())
    }

    fn spawn_container_process(
        &self,
        container_script: &Path,
        container_name: &str,
        image: &str,
        mounts: &[VolumeMount],
        env_vars: &[(String, String)],
        network: Option<&str>,
    ) -> Result<Child> {
        let ctx = EngineCommandContext {
            workdir: &self.config.workdir,
            container_root: Path::new(CONTAINER_WORKDIR),
            container_script,
            container_name,
            image,
            mounts,
            env_vars,
            network,
        };

        let mut command = match self.config.engine {
            EngineKind::ContainerCli => ContainerExecutor::build_command(&ctx),
            EngineKind::Docker => DockerExecutor::build_command(&ctx),
            EngineKind::Podman => PodmanExecutor::build_command(&ctx),
            EngineKind::Nerdctl => NerdctlExecutor::build_command(&ctx),
            EngineKind::Orbstack => OrbstackExecutor::build_command(&ctx),
        };

        command
            .spawn()
            .with_context(|| format!("failed to run {:?} command", self.config.engine))
    }

    pub(crate) fn kill_container(&self, job_name: &str, container_name: &str) {
        let binary = match self.config.engine {
            EngineKind::ContainerCli => "container",
            EngineKind::Docker | EngineKind::Orbstack => "docker",
            EngineKind::Podman => "podman",
            EngineKind::Nerdctl => "nerdctl",
        };
        let mut command = Command::new(binary);
        if binary == "container" {
            command.arg("rm").arg("--force").arg(container_name);
        } else {
            command.arg("rm").arg("-f").arg(container_name);
        }
        if let Err(err) = command.status() {
            warn!(
                job = job_name,
                container = container_name,
                error = %err,
                "failed to terminate container after timeout"
            );
        }
    }

    fn capture_child_output(
        &self,
        proc: &mut Child,
        job: &Job,
        log_path: &Path,
        ui: Option<&UiBridge>,
    ) -> Result<()> {
        let stdout = proc
            .stdout
            .take()
            .context("missing stdout from container process")?;
        let stderr = proc
            .stderr
            .take()
            .context("missing stderr from container process")?;

        let formatter = LogFormatter::new(self.use_color).with_secrets(&self.secrets);
        let line_prefix = formatter.line_prefix().to_string();
        let mut log_file = File::create(log_path)
            .with_context(|| format!("failed to create log at {}", log_path.display()))?;
        let mut display_line_no = 1usize;

        stream_lines(stdout, stderr, |line| {
            let timestamp = OffsetDateTime::now_utc()
                .format(TIMESTAMP_FORMAT)
                .unwrap_or_else(|_| "??????????".to_string());
            for fragment in sanitize_fragments(&line) {
                let masked = formatter.mask(&fragment);
                let plain_line =
                    logging::format_plain_log_line(&timestamp, display_line_no, masked.as_ref());
                if let Some(ui) = ui {
                    ui.job_log_line(&job.name, &plain_line);
                } else {
                    let decorated =
                        formatter.format_masked(&timestamp, display_line_no, masked.as_ref());
                    display::print_prefixed_line(&line_prefix, &decorated);
                }
                logging::write_log_line(
                    &mut log_file,
                    &timestamp,
                    display_line_no,
                    masked.as_ref(),
                )?;
                display_line_no += 1;
            }
            Ok(())
        })
    }
    fn resolve_job_image(&self, job: &Job) -> Result<String> {
        if let Some(image) = job.image.clone() {
            return Ok(image);
        }
        if let Some(image) = self.g.defaults.image.clone() {
            return Ok(image);
        }
        if let Some(image) = &self.config.image {
            return Ok(image.clone());
        }
        Err(anyhow!(
            "job '{}' has no image (use --base-image or set image in pipeline/job)",
            job.name
        ))
    }

    fn next_attempt(&self, job_name: &str) -> usize {
        let mut attempts = self
            .job_attempts
            .lock()
            .expect("job attempt tracker mutex poisoned");
        let entry = attempts.entry(job_name.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }

    fn job_container_name(&self, stage_name: &str, job: &Job, attempt: usize) -> String {
        format!(
            "opal-{}-{}-{}-{:02}",
            self.run_id,
            stage_name_slug(stage_name),
            job_name_slug(&job.name),
            attempt
        )
    }

    fn expanded_commands(&self, job: &Job) -> Vec<String> {
        let mut cmds = Vec::new();
        if job.inherit_default_before_script {
            cmds.extend(self.g.defaults.before_script.iter().cloned());
        }
        if let Some(custom) = &job.before_script {
            cmds.extend(custom.iter().cloned());
        }
        cmds.extend(job.commands.iter().cloned());
        if let Some(custom) = &job.after_script {
            cmds.extend(custom.iter().cloned());
        }
        if job.inherit_default_after_script {
            cmds.extend(self.g.defaults.after_script.iter().cloned());
        }
        cmds
    }

    fn job_env(&self, job: &Job) -> Vec<(String, String)> {
        build_job_env(
            &self.env_vars,
            &self.g.defaults.variables,
            job,
            &self.secrets,
            Path::new(CONTAINER_WORKDIR),
            &self.run_id,
            &self.host_env,
        )
    }

    fn job_services(&self, job: &Job) -> Vec<ServiceConfig> {
        if job.services.is_empty() {
            self.g.defaults.services.clone()
        } else {
            job.services.clone()
        }
    }

    fn display(&self) -> DisplayFormatter {
        DisplayFormatter::new(self.use_color)
    }

    fn job_log_info(&self, job: &Job) -> (PathBuf, String) {
        logging::job_log_info(&self.logs_dir, &self.run_id, job)
    }

    fn container_path_rel(&self, host_path: &Path) -> Result<PathBuf> {
        paths::to_container_path(host_path, &self.config.workdir)
    }
}

fn release_resource_lock(
    planned: &PlannedJob,
    ready: &mut VecDeque<String>,
    resource_locks: &mut HashMap<String, bool>,
    resource_waiting: &mut HashMap<String, VecDeque<String>>,
) {
    if let Some(group) = &planned.resource_group {
        resource_locks.insert(group.clone(), false);
        if let Some(queue) = resource_waiting.get_mut(group)
            && let Some(next) = queue.pop_front()
        {
            ready.push_back(next);
        }
    }
}

fn cache_policy_label(policy: CachePolicy) -> &'static str {
    match policy {
        CachePolicy::Pull => "pull",
        CachePolicy::Push => "push",
        CachePolicy::PullPush => "pull-push",
    }
}
