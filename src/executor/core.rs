use super::{
    paths, script, ContainerExecutor, DockerExecutor, NerdctlExecutor, OrbstackExecutor,
    PodmanExecutor,
};
use crate::artifacts::ArtifactManager;
use crate::cache::CacheManager;
use crate::display::{self, indent_block, print_pipeline_summary, DisplayFormatter};
use crate::engine::EngineCommandContext;
use crate::env::{build_job_env, collect_env_vars};
use crate::history::{self, HistoryEntry, HistoryJob, HistoryStatus};
use crate::logging::{self, sanitize_fragments, LogFormatter};
use crate::mounts::{self, VolumeMount};
use crate::naming::{job_name_slug, stage_name_slug, generate_run_id};
use crate::pipeline::{
    self, HaltKind, Job, JobEvent, JobPlan, JobRunInfo, JobStatus, JobSummary, PipelineGraph,
    PlannedJob, StageState,
};
use crate::runner::ExecuteContext;
use crate::secrets::SecretsStore;
use crate::terminal::{should_use_color, stream_lines};
use crate::ui::{UiBridge, UiCommand, UiHandle, UiJobInfo, UiJobStatus};
use crate::{EngineKind, ExecutorConfig};
use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};
use tokio::sync::{mpsc, Semaphore};
use tokio::task;
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
    stage_positions: HashMap<String, usize>,
    stage_states: Arc<Mutex<HashMap<String, StageState>>>,
    job_attempts: Arc<Mutex<HashMap<String, usize>>>,
    history_path: PathBuf,
    history_entries: Arc<Mutex<Vec<HistoryEntry>>>,
    secrets: SecretsStore,
    artifacts: ArtifactManager,
    cache: CacheManager,
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
        let env_vars = collect_env_vars(&config.env_includes)?;
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
            stage_positions,
            stage_states: Arc::new(Mutex::new(stage_states)),
            job_attempts: Arc::new(Mutex::new(HashMap::new())),
            history_path,
            history_entries: Arc::new(Mutex::new(history_entries)),
            secrets,
            artifacts,
            cache,
        })
    }

    pub async fn run(&self) -> Result<()> {
        let plan = self.plan_jobs()?;
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
            )?)
        } else {
            None
        };
        let mut command_rx = ui_handle
            .as_ref()
            .and_then(|handle| handle.command_receiver());
        let ui_bridge = ui_handle.as_ref().map(|handle| Arc::new(handle.bridge()));

        let (mut summaries, result) = self.execute_plan(&plan, ui_bridge.clone()).await;

        if let Some(handle) = &ui_handle {
            handle.pipeline_finished();
        }

        if let Some(commands) = command_rx.as_mut() {
            self.handle_restart_commands(&plan, ui_bridge.clone(), commands, &mut summaries)
                .await?;
        }

        let history_entry = self.record_pipeline_history(&summaries);
        if let (Some(entry), Some(ui)) = (history_entry, ui_bridge.as_deref()) {
            ui.history_updated(entry);
        }

        if let Some(handle) = ui_handle {
            handle.wait_for_exit();
        }

        let display = self.display();
        print_pipeline_summary(
            &display,
            &plan,
            &summaries,
            &self.session_dir,
            display::print_line,
        );
        result
    }

    fn plan_jobs(&self) -> Result<JobPlan> {
        let mut nodes = HashMap::new();
        let mut ordered = Vec::new();

        for (stage_idx, stage) in self.g.stages.iter().enumerate() {
            let default_deps: Vec<String> = if stage_idx == 0 {
                Vec::new()
            } else {
                self.g.stages[stage_idx - 1]
                    .jobs
                    .iter()
                    .map(|idx| self.g.graph[*idx].name.clone())
                    .collect()
            };

            for node_idx in &stage.jobs {
                let job = self
                    .g
                    .graph
                    .node_weight(*node_idx)
                    .cloned()
                    .ok_or_else(|| anyhow!("missing job for node"))?;

                let mut deps = if !job.needs.is_empty() {
                    job.needs.iter().map(|need| need.job.clone()).collect()
                } else {
                    default_deps.clone()
                };
                deps.sort();
                deps.dedup();

               let (log_path, log_hash) = self.job_log_info(&job);
                ordered.push(job.name.clone());
                nodes.insert(
                    job.name.clone(),
                    PlannedJob {
                        job,
                        stage_name: stage.name.clone(),
                        dependencies: deps,
                        log_path,
                        log_hash,
                    },
                );
            }
        }

        let mut order_index = HashMap::new();
        for (idx, name) in ordered.iter().enumerate() {
            order_index.insert(name.clone(), idx);
        }

        let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
        for (name, planned) in &nodes {
            for dep in &planned.dependencies {
                if !nodes.contains_key(dep) {
                    return Err(anyhow!("job '{}' depends on unknown job '{}'", name, dep));
                }
                dependents
                    .entry(dep.clone())
                    .or_default()
                    .push(name.clone());
            }
        }

        for deps in dependents.values_mut() {
            deps.sort_by_key(|name| order_index.get(name).copied().unwrap_or(usize::MAX));
        }

        Ok(JobPlan {
            ordered,
            nodes,
            dependents,
            order_index,
        })
    }

    async fn execute_plan(
        &self,
        plan: &JobPlan,
        ui: Option<Arc<UiBridge>>,
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
        for name in &plan.ordered {
            if remaining.get(name).copied().unwrap_or(0) == 0 {
                ready.push_back(name.clone());
            }
        }

        if ready.is_empty() {
            return (
                Vec::new(),
                Err(anyhow!("no runnable jobs (cyclic dependencies?)")),
            );
        }

        let semaphore = Arc::new(Semaphore::new(self.config.max_parallel_jobs.max(1)));
        let exec = Arc::new(self.clone());
        let (tx, mut rx) = mpsc::unbounded_channel::<JobEvent>();
        let mut running = HashSet::new();
        let mut completed = 0usize;
        let mut job_failure: Option<(String, anyhow::Error)> = None;
        let mut halt_error: Option<anyhow::Error> = None;
        let mut halt_kind = HaltKind::None;
        let mut summaries: Vec<JobSummary> = Vec::new();

        while completed < total {
            if job_failure.is_none() {
                while let Some(name) = ready.pop_front() {
                    if running.contains(&name) {
                        continue;
                    }
                    if let Some(planned) = plan.nodes.get(&name).cloned() {
                        let run_info = self.log_job_start(&planned, ui.as_deref());
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
                }
            }

            if running.is_empty() {
                if completed == total {
                    break;
                }
                if let Some((name, err)) = job_failure.take() {
                    halt_kind = HaltKind::JobFailure;
                    halt_error = Some(err.context(format!("job '{name}' failed")));
                    break;
                }
                let remaining_jobs: Vec<_> = remaining
                    .iter()
                    .filter_map(|(name, &count)| if count > 0 { Some(name.clone()) } else { None })
                    .collect();
                halt_kind = HaltKind::Deadlock;
                halt_error = Some(anyhow!(
                    "no runnable jobs, potential dependency cycle involving: {:?}",
                    remaining_jobs
                ));
                break;
            }

            let Some(event) = rx.recv().await else {
                halt_kind = HaltKind::ChannelClosed;
                halt_error = Some(anyhow!(
                    "job worker channel closed unexpectedly while {} jobs remained",
                    total - completed
                ));
                break;
            };

            running.remove(&event.name);
            completed += 1;

            match event.result {
                Ok(_) => {
                    if let Some(children) = plan.dependents.get(&event.name) {
                        for child in children {
                            if let Some(count) = remaining.get_mut(child)
                                && *count > 0
                            {
                                *count -= 1;
                                if *count == 0 {
                                    ready.push_back(child.clone());
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
                    });
                }
                Err(err) => {
                    let err_msg = err.to_string();
                    job_failure = job_failure.or(Some((event.name.clone(), err)));
                    summaries.push(JobSummary {
                        name: event.name.clone(),
                        stage_name: event.stage_name.clone(),
                        duration: event.duration,
                        status: JobStatus::Failed(err_msg),
                        log_path: event.log_path.clone(),
                        log_hash: event.log_hash.clone(),
                    });
                }
            }
        }

        if halt_kind != HaltKind::None && summaries.len() < total {
            let mut recorded: HashSet<String> =
                summaries.iter().map(|entry| entry.name.clone()).collect();
            let skip_reason = match halt_kind {
                HaltKind::JobFailure => {
                    Some("not run (pipeline stopped after failure)".to_string())
                }
                HaltKind::Deadlock => Some("not run (dependency cycle detected)".to_string()),
                HaltKind::ChannelClosed => {
                    Some("not run (executor channel closed unexpectedly)".to_string())
                }
                HaltKind::None => None,
            };

            if let Some(reason) = skip_reason {
                for job_name in &plan.ordered {
                    if recorded.contains(job_name) {
                        continue;
                    }
                    if let Some(planned) = plan.nodes.get(job_name) {
                        if let Some(ui) = ui.as_deref() {
                            ui.job_finished(
                                job_name,
                                UiJobStatus::Skipped,
                                0.0,
                                Some(reason.clone()),
                            );
                        }
                        summaries.push(JobSummary {
                            name: job_name.clone(),
                            stage_name: planned.stage_name.clone(),
                            duration: 0.0,
                            status: JobStatus::Skipped(reason.clone()),
                            log_path: Some(planned.log_path.clone()),
                            log_hash: planned.log_hash.clone(),
                        });
                        recorded.insert(job_name.clone());
                    }
                }
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

                    let run_info = self.log_job_start(&planned, ui.as_deref());
                    let exec = self.clone();
                    let ui_clone = ui.clone();
                    let run_info_clone = run_info.clone();
                    let event = task::spawn_blocking(move || {
                        exec.run_planned_job(planned, run_info_clone, ui_clone)
                    })
                    .await
                    .context("job restart task failed")?;
                    self.update_summaries_from_event(event, summaries);
                }
            }
        }
        Ok(())
    }

    fn update_summaries_from_event(&self, event: JobEvent, summaries: &mut Vec<JobSummary>) {
        let JobEvent {
            name,
            stage_name,
            duration,
            log_path,
            log_hash,
            result,
        } = event;

        let status = match result {
            Ok(_) => JobStatus::Success,
            Err(err) => JobStatus::Failed(err.to_string()),
        };

        summaries.retain(|entry| entry.name != name);
        summaries.push(JobSummary {
            name,
            stage_name,
            duration,
            status,
            log_path,
            log_hash,
        });
    }

    fn record_pipeline_history(&self, summaries: &[JobSummary]) -> Option<HistoryEntry> {
        let finished_at = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".to_string());
        let pipeline_status = if summaries
            .iter()
            .any(|entry| matches!(entry.status, JobStatus::Failed(_)))
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

    fn log_job_start(&self, planned: &PlannedJob, ui: Option<&UiBridge>) -> JobRunInfo {
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

            let job_image = self.resolve_job_image(job);
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

        JobRunInfo { container_name }
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
            let cache_env: HashMap<String, String> =
                env_vars.iter().cloned().collect();
            let mut mounts = mounts::collect_volume_mounts(
                &job,
                &self.g,
                &self.artifacts,
                &self.cache,
                &cache_env,
                Path::new(CONTAINER_WORKDIR),
            )?;
            if let Some((host, container_path)) = self.secrets.volume_mount() {
                mounts.push(VolumeMount {
                    host,
                    container: container_path,
                    read_only: true,
                });
            }
            let job_image = self.resolve_job_image(&job);
            let container_name = run_info.container_name.clone();
            let script_commands = self.expanded_commands(&job);
            let script_path = script::write_job_script(
                &self.scripts_dir,
                &job,
                &script_commands,
                self.verbose_scripts,
            )?;
            self.execute(ExecuteContext {
                script_path: &script_path,
                log_path: &log_path,
                mounts: &mounts,
                image: &job_image,
                container_name: &container_name,
                job: &job,
                ui: ui_ref,
                env_vars: &env_vars,
            })?;
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
        if let Some(ui) = ui_ref {
            match &result {
                Ok(_) => ui.job_finished(&job_name, UiJobStatus::Success, duration, None),
                Err(err) => ui.job_finished(
                    &job_name,
                    UiJobStatus::Failed,
                    duration,
                    Some(err.to_string()),
                ),
            }
        }

        JobEvent {
            name: job_name,
            stage_name,
            duration,
            log_path: Some(log_path.clone()),
            log_hash,
            result,
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
    ) -> Result<Child> {
        let ctx = EngineCommandContext {
            workdir: &self.config.workdir,
            container_root: Path::new(CONTAINER_WORKDIR),
            container_script,
            container_name,
            image,
            mounts,
            env_vars,
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
    fn resolve_job_image(&self, job: &Job) -> String {
        job.image
            .clone()
            .or_else(|| self.g.defaults.image.clone())
            .unwrap_or_else(|| self.config.image.clone())
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
        cmds.extend(self.g.defaults.before_script.iter().cloned());
        cmds.extend(job.commands.iter().cloned());
        cmds.extend(self.g.defaults.after_script.iter().cloned());
        cmds
    }

    fn job_env(&self, job: &Job) -> Vec<(String, String)> {
        build_job_env(
            &self.env_vars,
            &self.g.defaults.variables,
            job,
            &self.secrets,
            &self.config.workdir,
            &self.run_id,
        )
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
