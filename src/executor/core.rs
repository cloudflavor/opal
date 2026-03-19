use super::{
    ContainerExecutor, DockerExecutor, NerdctlExecutor, OrbstackExecutor, PodmanExecutor,
    job_runner::PreparedJobRun, orchestrator, paths, script, services::ServiceRuntime,
};
use crate::config::ResolvedRegistryAuth;
use crate::display::{
    self, DisplayFormatter, collect_pipeline_plan, indent_block, print_pipeline_summary,
};
use crate::engine::EngineCommandContext;
use crate::env::{build_job_env, collect_env_vars, expand_env_list, expand_value};
use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::history::{self, HistoryCache, HistoryEntry, HistoryJob, HistoryStatus};
use crate::logging::{self, LogFormatter, sanitize_fragments};
use crate::model::{CachePolicySpec, JobSpec, PipelineSpec, ServiceSpec};
use crate::naming::{generate_run_id, job_name_slug, stage_name_slug};
use crate::pipeline::{
    self, ArtifactManager, CacheManager, ExternalArtifactsManager, JobRunInfo, JobStatus,
    JobSummary, RuleContext, StageState, VolumeMount, mounts,
};
use crate::runner::ExecuteContext;
use crate::secrets::SecretsStore;
use crate::terminal::{should_use_color, stream_lines};
use crate::ui::{UiBridge, UiHandle, UiJobInfo, UiJobResources};
use crate::{EngineKind, ExecutorConfig, runtime};
use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};
use tracing::warn;

pub(super) const CONTAINER_ROOT: &str = "/builds";
const TIMESTAMP_FORMAT: &[FormatItem<'static>] =
    format_description!("[hour]:[minute]:[second].[subsecond digits:3]");
const MAX_CONTAINER_NAME: usize = 63;

#[derive(Debug, Clone)]
pub struct ExecutorCore {
    pub config: ExecutorConfig,
    pipeline: PipelineSpec,
    use_color: bool,
    scripts_dir: PathBuf,
    logs_dir: PathBuf,
    session_dir: PathBuf,
    container_session_dir: PathBuf,
    run_id: String,
    verbose_scripts: bool,
    env_vars: Vec<(String, String)>,
    shared_env: HashMap<String, String>,
    container_workdir: PathBuf,
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
        let pipeline = PipelineSpec::from_path(&config.pipeline)?;
        let run_id = generate_run_id(&config);
        let runs_root = runtime::runs_root();
        fs::create_dir_all(&runs_root)
            .with_context(|| format!("failed to create {:?}", runs_root))?;

        let session_dir = runtime::session_dir(&run_id);
        if session_dir.exists() {
            fs::remove_dir_all(&session_dir)
                .with_context(|| format!("failed to clean {:?}", session_dir))?;
        }
        fs::create_dir_all(&session_dir)
            .with_context(|| format!("failed to create {:?}", session_dir))?;

        let scripts_dir = session_dir.join("scripts");
        fs::create_dir_all(&scripts_dir)
            .with_context(|| format!("failed to create {:?}", scripts_dir))?;

        let logs_dir = runtime::logs_dir(&run_id);
        fs::create_dir_all(&logs_dir)
            .with_context(|| format!("failed to create {:?}", logs_dir))?;

        let history_path = runtime::history_path();
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
        let env_verbose = env::var_os("OPAL_DEBUG")
            .map(|val| {
                let s = val.to_string_lossy();
                s == "1" || s.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false);
        let verbose_scripts = config.trace_scripts || env_verbose;
        let mut env_vars = collect_env_vars(&config.env_includes)?;
        let mut shared_env: HashMap<String, String> = env::vars().collect();
        expand_env_list(&mut env_vars[..], &shared_env);
        shared_env.extend(env_vars.iter().cloned());
        let mut stage_positions = HashMap::new();
        let mut stage_states = HashMap::new();
        for (idx, stage) in pipeline.stages.iter().enumerate() {
            stage_positions.insert(stage.name.clone(), idx);
            stage_states.insert(stage.name.clone(), StageState::new(stage.jobs.len()));
        }

        let secrets = SecretsStore::load(&config.workdir)?;
        let artifacts = ArtifactManager::new(session_dir.clone());
        let cache_root = runtime::cache_root();
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

        let project_dir = config
            .workdir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project");
        let container_workdir = Path::new(CONTAINER_ROOT).join(project_dir);
        let container_session_dir = Path::new("/opal").join(&run_id);

        let core = Self {
            config,
            pipeline,
            use_color,
            scripts_dir,
            logs_dir,
            session_dir,
            container_session_dir,
            run_id,
            verbose_scripts,
            env_vars,
            shared_env,
            container_workdir,
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
        };

        core.ensure_registry_logins()?;

        Ok(core)
    }

    pub async fn run(&self) -> Result<()> {
        let plan = Arc::new(self.plan_jobs()?);
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
                    name: planned.instance.job.name.clone(),
                    stage: planned.instance.stage_name.clone(),
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

        let (mut summaries, result) =
            orchestrator::execute_plan(self, plan.clone(), ui_bridge.clone(), command_rx.as_mut())
                .await;

        if let Some(handle) = &ui_handle {
            handle.pipeline_finished();
        }

        if let Some(commands) = command_rx.as_mut() {
            orchestrator::handle_restart_commands(
                self,
                plan.clone(),
                ui_bridge.clone(),
                commands,
                &mut summaries,
            )
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

    fn plan_jobs(&self) -> Result<ExecutionPlan> {
        let ctx = RuleContext::new(&self.config.workdir);
        if !pipeline::rules::filters_allow(&self.pipeline.filters, &ctx) {
            return Ok(ExecutionPlan {
                ordered: Vec::new(),
                nodes: HashMap::new(),
                dependents: HashMap::new(),
                order_index: HashMap::new(),
                variants: HashMap::new(),
            });
        }
        if let Some(workflow) = &self.pipeline.workflow
            && !pipeline::rules::evaluate_workflow(&workflow.rules, &ctx)?
        {
            return Ok(ExecutionPlan {
                ordered: Vec::new(),
                nodes: HashMap::new(),
                dependents: HashMap::new(),
                order_index: HashMap::new(),
                variants: HashMap::new(),
            });
        }
        pipeline::build_job_plan(&self.pipeline, Some(&ctx), |job| self.job_log_info(job))
    }

    fn collect_job_resources(&self, plan: &ExecutionPlan) -> HashMap<String, JobResourceInfo> {
        plan.nodes
            .values()
            .map(|planned| {
                let artifact_dir = if planned.instance.job.artifacts.paths.is_empty() {
                    None
                } else {
                    Some(
                        self.artifacts
                            .job_artifacts_root(&planned.instance.job.name)
                            .display()
                            .to_string(),
                    )
                };
                let artifact_paths = planned
                    .instance
                    .job
                    .artifacts
                    .paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect();
                let env_vars = self.job_env(&planned.instance.job);
                let cache_env: HashMap<String, String> = env_vars.iter().cloned().collect();
                let caches = self
                    .cache
                    .describe_entries(&planned.instance.job.cache, &cache_env)
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
                    planned.instance.job.name.clone(),
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

    pub(crate) fn log_job_start(
        &self,
        planned: &ExecutableJob,
        ui: Option<&UiBridge>,
    ) -> Result<JobRunInfo> {
        let attempt = self.next_attempt(&planned.instance.job.name);
        let container_name =
            self.job_container_name(&planned.instance.stage_name, &planned.instance.job, attempt);
        if let Some(ui) = ui {
            ui.job_started(&planned.instance.job.name);
        }

        if !self.config.enable_tui {
            let display = self.display();
            if self.stage_started(&planned.instance.stage_name) {
                if self.stage_position(&planned.instance.stage_name) > 0 {
                    display::print_blank_line();
                }
                display::print_line(display.stage_header(&planned.instance.stage_name));
            }

            let job = &planned.instance.job;
            let job_label = display.bold_green("  job:");
            let job_name = display.bold_white(job.name.as_str());
            display::print_line(format!("{} {}", job_label, job_name));

            if let Some(needs) = display.format_needs(job) {
                let needs_label = display.bold_cyan("    needs:");
                display::print_line(format!("{} {}", needs_label, needs));
            }
            if let Some(paths) = display.format_paths(&job.artifacts.paths) {
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

        self.track_running_container(&planned.instance.job.name, &container_name);
        Ok(JobRunInfo { container_name })
    }

    pub(crate) fn prepare_job_run(
        &self,
        plan: &ExecutionPlan,
        job: &JobSpec,
    ) -> Result<PreparedJobRun> {
        self.artifacts.prepare_targets(job)?;
        let mut env_vars = self.job_env(job);
        let cache_env: HashMap<String, String> = env_vars.iter().cloned().collect();
        let service_configs = self.job_services(job);
        let service_runtime = ServiceRuntime::start(
            self.config.engine,
            &self.run_id,
            &job.name,
            &service_configs,
            &env_vars,
            &self.shared_env,
        )?;
        if let Some(runtime) = service_runtime.as_ref() {
            env_vars.extend(runtime.link_env().iter().cloned());
        }
        let mut mounts = mounts::collect_volume_mounts(mounts::VolumeMountContext {
            job,
            plan,
            pipeline: &self.pipeline,
            artifacts: &self.artifacts,
            cache: &self.cache,
            cache_env: &cache_env,
            container_root: &self.container_workdir,
            external: self.external_artifacts.as_ref(),
        })?;
        mounts.push(VolumeMount {
            host: self.session_dir.clone(),
            container: self.container_session_dir.clone(),
            read_only: false,
        });
        if let Some((host, container_path)) = self.secrets.volume_mount() {
            mounts.push(VolumeMount {
                host,
                container: container_path,
                read_only: true,
            });
        }
        let job_image = self.resolve_job_image_with_env(job, Some(&cache_env))?;
        let script_commands = self.expanded_commands(job);
        let script_path = script::write_job_script(
            &self.scripts_dir,
            &self.container_workdir,
            job,
            &script_commands,
            self.verbose_scripts,
        )?;

        Ok(PreparedJobRun {
            env_vars,
            service_runtime,
            mounts,
            job_image,
            script_path,
        })
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

    pub(crate) fn clear_running_container(&self, job_name: &str) {
        if let Ok(mut map) = self.running_containers.lock() {
            map.remove(job_name);
        }
    }

    fn mark_job_cancelled(&self, job_name: &str) {
        if let Ok(mut cancelled) = self.cancelled_jobs.lock() {
            cancelled.insert(job_name.to_string());
        }
    }

    pub(crate) fn take_cancelled_job(&self, job_name: &str) -> bool {
        if let Ok(mut cancelled) = self.cancelled_jobs.lock() {
            cancelled.remove(job_name)
        } else {
            false
        }
    }

    pub(crate) fn cancel_running_job(&self, job_name: &str) -> bool {
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

    pub(crate) fn cancel_all_running_jobs(&self) {
        let containers: Vec<(String, String)> = match self.running_containers.lock() {
            Ok(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Err(_) => return,
        };
        for (job, container) in containers {
            self.mark_job_cancelled(&job);
            self.kill_container(&job, &container);
        }
    }

    pub(crate) fn execute(&self, ctx: ExecuteContext<'_>) -> Result<()> {
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

    pub(crate) fn print_job_completion(
        &self,
        stage_name: &str,
        script_path: &Path,
        log_path: &Path,
        elapsed: f32,
    ) {
        if !self.config.enable_tui {
            let display = self.display();
            display::print_line(format!("    script stored at {}", script_path.display()));
            display::print_line(format!("    log file stored at {}", log_path.display()));
            let finish_label = display.bold_green("    ✓ finished in");
            display::print_line(format!("{} {:.2}s", finish_label, elapsed));

            if let Some(stage_elapsed) = self.stage_job_completed(stage_name) {
                let stage_footer = display.bold_blue("╰─ stage complete in");
                display::print_line(format!("{stage_footer} {:.2}s", stage_elapsed));
            }
        }
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
        let container_cfg = self.config.settings.container_settings();
        let ctx = EngineCommandContext {
            workdir: &self.config.workdir,
            container_root: &self.container_workdir,
            container_script,
            container_name,
            image,
            mounts,
            env_vars,
            network,
            cpus: container_cfg.and_then(|cfg| cfg.cpus.as_deref()),
            memory: container_cfg.and_then(|cfg| cfg.memory.as_deref()),
            dns: container_cfg.and_then(|cfg| cfg.dns.as_deref()),
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

    fn ensure_registry_logins(&self) -> Result<()> {
        let auths = self.config.settings.registry_auth_for(self.config.engine)?;
        for auth in &auths {
            match self.config.engine {
                EngineKind::ContainerCli => self.container_registry_login(auth)?,
                EngineKind::Docker | EngineKind::Orbstack => {
                    self.standard_registry_login("docker", auth)?
                }
                EngineKind::Podman => self.standard_registry_login("podman", auth)?,
                EngineKind::Nerdctl => self.standard_registry_login("nerdctl", auth)?,
            }
        }
        Ok(())
    }

    fn container_registry_login(&self, auth: &ResolvedRegistryAuth) -> Result<()> {
        let mut command = Command::new("container");
        command.arg("registry").arg("login");
        if let Some(scheme) = auth.scheme.as_deref() {
            command.arg("--scheme").arg(scheme);
        }
        command
            .arg("--username")
            .arg(&auth.username)
            .arg("--password-stdin")
            .arg(&auth.server)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().with_context(|| {
            format!("failed to run container registry login for {}", auth.server)
        })?;
        child
            .stdin
            .as_mut()
            .context("missing stdin for container registry login")?
            .write_all(auth.password.as_bytes())?;
        let status = child.wait()?;
        if !status.success() {
            return Err(anyhow!(
                "container registry login for {} failed with status {:?}",
                auth.server,
                status.code()
            ));
        }
        Ok(())
    }

    fn standard_registry_login(&self, binary: &str, auth: &ResolvedRegistryAuth) -> Result<()> {
        let mut command = Command::new(binary);
        command
            .arg("login")
            .arg("--username")
            .arg(&auth.username)
            .arg("--password-stdin")
            .arg(&auth.server)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to run {} login for {}", binary, auth.server))?;
        child
            .stdin
            .as_mut()
            .context("missing stdin for registry login")?
            .write_all(auth.password.as_bytes())?;
        let status = child.wait()?;
        if !status.success() {
            return Err(anyhow!(
                "{} login for {} failed with status {:?}",
                binary,
                auth.server,
                status.code()
            ));
        }
        Ok(())
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
        job: &JobSpec,
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
    fn resolve_job_image(&self, job: &JobSpec) -> Result<String> {
        self.resolve_job_image_with_env(job, None)
    }

    fn resolve_job_image_with_env(
        &self,
        job: &JobSpec,
        env_lookup: Option<&HashMap<String, String>>,
    ) -> Result<String> {
        let template = if let Some(image) = job.image.as_ref() {
            image.clone()
        } else if let Some(image) = self.pipeline.defaults.image.as_ref() {
            image.clone()
        } else if let Some(image) = self.config.image.clone() {
            image
        } else {
            return Err(anyhow!(
                "job '{}' has no image (use --base-image or set image in pipeline/job)",
                job.name
            ));
        };

        if !template.contains('$') {
            return Ok(template);
        }

        if let Some(map) = env_lookup {
            Ok(expand_value(&template, map))
        } else {
            let owned_lookup: HashMap<String, String> = self.job_env(job).into_iter().collect();
            Ok(expand_value(&template, &owned_lookup))
        }
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

    fn job_container_name(&self, stage_name: &str, job: &JobSpec, attempt: usize) -> String {
        let base = format!(
            "opal-{}-{}-{}-{:02}",
            self.run_id,
            stage_name_slug(stage_name),
            job_name_slug(&job.name),
            attempt
        );
        if base.len() <= MAX_CONTAINER_NAME {
            return base;
        }
        self.short_container_name(stage_name, job, attempt)
    }

    fn short_container_name(&self, stage_name: &str, job: &JobSpec, attempt: usize) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.run_id.as_bytes());
        hasher.update(stage_name.as_bytes());
        hasher.update(job.name.as_bytes());
        let digest = hasher.finalize();
        let mut short = String::with_capacity(16);
        for byte in digest.iter().take(6) {
            let _ = FmtWrite::write_fmt(&mut short, format_args!("{:02x}", byte));
        }
        format!("opal-{short}-{:02}", attempt)
    }

    fn expanded_commands(&self, job: &JobSpec) -> Vec<String> {
        let mut cmds = Vec::new();
        if job.inherit_default_before_script {
            cmds.extend(self.pipeline.defaults.before_script.iter().cloned());
        }
        if let Some(custom) = &job.before_script {
            cmds.extend(custom.iter().cloned());
        }
        cmds.extend(job.commands.iter().cloned());
        if let Some(custom) = &job.after_script {
            cmds.extend(custom.iter().cloned());
        }
        if job.inherit_default_after_script {
            cmds.extend(self.pipeline.defaults.after_script.iter().cloned());
        }
        cmds
    }

    fn job_env(&self, job: &JobSpec) -> Vec<(String, String)> {
        build_job_env(
            &self.env_vars,
            &self.pipeline.defaults.variables,
            job,
            &self.secrets,
            &self.config.workdir,
            &self.container_workdir,
            Path::new(CONTAINER_ROOT),
            &self.run_id,
            &self.shared_env,
        )
    }

    fn job_services(&self, job: &JobSpec) -> Vec<ServiceSpec> {
        if job.services.is_empty() {
            self.pipeline.defaults.services.clone()
        } else {
            job.services.clone()
        }
    }

    fn display(&self) -> DisplayFormatter {
        DisplayFormatter::new(self.use_color)
    }

    fn job_log_info(&self, job: &JobSpec) -> (PathBuf, String) {
        logging::job_log_info(&self.logs_dir, &self.run_id, job)
    }

    fn container_path_rel(&self, host_path: &Path) -> Result<PathBuf> {
        paths::to_container_path(
            host_path,
            &[
                (&*self.config.workdir, &*self.container_workdir),
                (&*self.session_dir, &*self.container_session_dir),
            ],
        )
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

fn cache_policy_label(policy: CachePolicySpec) -> &'static str {
    match policy {
        CachePolicySpec::Pull => "pull",
        CachePolicySpec::Push => "push",
        CachePolicySpec::PullPush => "pull-push",
    }
}
