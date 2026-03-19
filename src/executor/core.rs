mod history_store;
mod lifecycle;
mod preparer;
mod process;
mod registry;
mod runtime_state;
mod stage_tracker;

use super::{orchestrator, paths};
use crate::display::{
    self, DisplayFormatter, collect_pipeline_plan, indent_block, print_pipeline_summary,
};
use crate::env::{build_job_env, collect_env_vars, expand_env_list, expand_value};
use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::history::{HistoryCache, HistoryEntry};
use crate::logging;
use crate::model::{CachePolicySpec, JobSpec, PipelineSpec};
use crate::naming::{generate_run_id, job_name_slug, stage_name_slug};
use crate::pipeline::{
    self, ArtifactManager, CacheManager, ExternalArtifactsManager, JobRunInfo, JobSummary,
    RuleContext,
};
use crate::runner::ExecuteContext;
use crate::secrets::SecretsStore;
use crate::terminal::should_use_color;
use crate::ui::{UiBridge, UiHandle, UiJobInfo, UiJobResources};
use crate::{ExecutorConfig, runtime};
use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) const CONTAINER_ROOT: &str = "/builds";
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
    stage_tracker: stage_tracker::StageTracker,
    runtime_state: runtime_state::RuntimeState,
    history_store: history_store::HistoryStore,
    secrets: SecretsStore,
    artifacts: ArtifactManager,
    cache: CacheManager,
    external_artifacts: Option<ExternalArtifactsManager>,
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

        let history_store = history_store::HistoryStore::load(runtime::history_path());

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
        let stage_specs: Vec<(String, usize)> = pipeline
            .stages
            .iter()
            .map(|stage| (stage.name.clone(), stage.jobs.len()))
            .collect();
        let stage_tracker = stage_tracker::StageTracker::new(&stage_specs);

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
            stage_tracker,
            runtime_state: runtime_state::RuntimeState::default(),
            history_store,
            secrets,
            artifacts,
            cache,
            external_artifacts,
        };

        registry::ensure_registry_logins(&core)?;

        Ok(core)
    }

    pub async fn run(&self) -> Result<()> {
        let plan = Arc::new(self.plan_jobs()?);
        let resource_map = self.collect_job_resources(&plan);
        let display = self.display();
        let plan_text = collect_pipeline_plan(&display, &plan).join("\n");
        let ui_resources = Self::convert_ui_resources(&resource_map);
        let history_snapshot = self.history_store.snapshot();
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
        let resource_map = resources
            .iter()
            .map(|(name, info)| {
                (
                    name.clone(),
                    history_store::HistoryResources {
                        artifact_dir: info.artifact_dir.clone(),
                        artifacts: info.artifact_paths.clone(),
                        caches: info.caches.clone(),
                    },
                )
            })
            .collect();
        self.history_store
            .record(&self.run_id, summaries, &resource_map)
    }

    pub(crate) fn log_job_start(
        &self,
        planned: &ExecutableJob,
        ui: Option<&UiBridge>,
    ) -> Result<JobRunInfo> {
        let attempt = self.runtime_state.next_attempt(&planned.instance.job.name);
        let container_name =
            self.job_container_name(&planned.instance.stage_name, &planned.instance.job, attempt);
        if let Some(ui) = ui {
            ui.job_started(&planned.instance.job.name);
        }

        if !self.config.enable_tui {
            let display = self.display();
            if self.stage_tracker.start(&planned.instance.stage_name) {
                if self.stage_tracker.position(&planned.instance.stage_name) > 0 {
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

        self.runtime_state
            .track_running_container(&planned.instance.job.name, &container_name);
        Ok(JobRunInfo { container_name })
    }

    pub(crate) fn prepare_job_run(
        &self,
        plan: &ExecutionPlan,
        job: &JobSpec,
    ) -> Result<preparer::PreparedJobRun> {
        preparer::prepare_job_run(self, plan, job)
    }

    pub(crate) fn clear_running_container(&self, job_name: &str) {
        self.runtime_state.clear_running_container(job_name);
    }

    pub(crate) fn take_cancelled_job(&self, job_name: &str) -> bool {
        self.runtime_state.take_cancelled_job(job_name)
    }

    pub(crate) fn cancel_running_job(&self, job_name: &str) -> bool {
        let container = self.runtime_state.running_container(job_name);
        if let Some(container_name) = container {
            self.runtime_state.mark_job_cancelled(job_name);
            self.kill_container(job_name, &container_name);
            true
        } else {
            false
        }
    }

    pub(crate) fn cancel_all_running_jobs(&self) {
        for (job, container) in self.runtime_state.running_containers() {
            self.runtime_state.mark_job_cancelled(&job);
            self.kill_container(&job, &container);
        }
    }

    pub(crate) fn execute(&self, ctx: ExecuteContext<'_>) -> Result<()> {
        process::execute(self, ctx)
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

            if let Some(stage_elapsed) = self.stage_tracker.complete_job(stage_name) {
                let stage_footer = display.bold_blue("╰─ stage complete in");
                display::print_line(format!("{stage_footer} {:.2}s", stage_elapsed));
            }
        }
    }

    pub(crate) fn kill_container(&self, job_name: &str, container_name: &str) {
        lifecycle::kill_container(self, job_name, container_name);
    }

    pub(crate) fn cleanup_finished_container(&self, container_name: &str) {
        lifecycle::cleanup_finished_container(self, container_name);
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

    pub(crate) fn expanded_environment(
        &self,
        job: &JobSpec,
    ) -> Option<crate::model::EnvironmentSpec> {
        let environment = job.environment.as_ref()?;
        let lookup: HashMap<String, String> = self.job_env(job).into_iter().collect();
        Some(crate::env::expand_environment(environment, &lookup))
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

fn cache_policy_label(policy: CachePolicySpec) -> &'static str {
    match policy {
        CachePolicySpec::Pull => "pull",
        CachePolicySpec::Push => "push",
        CachePolicySpec::PullPush => "pull-push",
    }
}

#[cfg(test)]
mod tests {
    // ExecutorCore-specific unit coverage lives in child modules while phase 3 extraction continues.
}
