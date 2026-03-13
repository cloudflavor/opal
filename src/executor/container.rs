use super::ui::{UiBridge, UiHandle, UiJobInfo, UiJobStatus};
use crate::ExecutorConfig;
use crate::pipeline::{Job, PipelineGraph};
use anyhow::{Context, Result, anyhow};
use globset::{Glob, GlobSetBuilder};
use owo_colors::OwoColorize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use time::format_description::FormatItem;
use time::{OffsetDateTime, macros::format_description};
use tokio::sync::{Semaphore, mpsc};
use tokio::task;
use tracing::warn;

const CONTAINER_WORKDIR: &str = "/workspace";
const TIMESTAMP_FORMAT: &[FormatItem<'static>] =
    format_description!("[hour]:[minute]:[second].[subsecond digits:3]");

#[derive(Debug, Clone)]
struct VolumeMount {
    host: PathBuf,
    container: PathBuf,
    read_only: bool,
}

#[derive(Debug, Clone)]
pub struct ContainerExecutor {
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
}

impl ContainerExecutor {
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
        })
    }

    pub async fn run(&self) -> Result<()> {
        let plan = self.plan_jobs()?;
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
            Some(UiHandle::start(jobs)?)
        } else {
            None
        };
        let ui_bridge = ui_handle.as_ref().map(|handle| Arc::new(handle.bridge()));

        let (summaries, result) = self.execute_plan(&plan, ui_bridge.clone()).await;

        if let Some(handle) = &ui_handle {
            handle.pipeline_finished();
        }

        if let Some(handle) = ui_handle {
            handle.wait_for_exit();
        }

        self.print_summary(&plan, &summaries);
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
                        self.log_job_start(&planned, ui.as_deref());
                        running.insert(name.clone());
                        spawn_job(
                            exec.clone(),
                            planned,
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
                            if let Some(count) = remaining.get_mut(child) {
                                if *count > 0 {
                                    *count -= 1;
                                    if *count == 0 {
                                        ready.push_back(child.clone());
                                    }
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

    fn print_summary(&self, plan: &JobPlan, summaries: &[JobSummary]) {
        println!();
        let header = self.colorize("╭─ pipeline summary", |t| {
            format!("{}", t.bold().blue())
        });
        self.emit_line(header);

        if summaries.is_empty() {
            self.emit_line("  no jobs were executed".to_string());
            self.emit_line(format!("  session data: {}", self.session_dir.display()));
            return;
        }

        let mut ordered = summaries.to_vec();
        ordered.sort_by_key(|entry| {
            plan.order_index
                .get(&entry.name)
                .copied()
                .unwrap_or(usize::MAX)
        });

        let mut success = 0usize;
        let mut failed = 0usize;
        let mut skipped = 0usize;

        for entry in &ordered {
            match &entry.status {
                JobStatus::Success => success += 1,
                JobStatus::Failed(_) => failed += 1,
                JobStatus::Skipped(_) => skipped += 1,
            }
            let icon = match &entry.status {
                JobStatus::Success => self.colorize("✓", |t| format!("{}", t.bold().green())),
                JobStatus::Failed(_) => self.colorize("✗", |t| format!("{}", t.bold().red())),
                JobStatus::Skipped(_) => self.colorize("•", |t| format!("{}", t.bold().yellow())),
            };
            let mut line = format!(
                "  {} {} (stage {}, log {})",
                icon, entry.name, entry.stage_name, entry.log_hash
            );
            match &entry.status {
                JobStatus::Success => {
                    line.push_str(&format!(" – {:.2}s", entry.duration));
                }
                JobStatus::Failed(msg) => {
                    line.push_str(&format!(" – {:.2}s failed: {}", entry.duration, msg));
                }
                JobStatus::Skipped(msg) => {
                    line.push_str(&format!(" – {}", msg));
                }
            }
            if let Some(log_path) = &entry.log_path {
                line.push_str(&format!(" [log: {}]", log_path.display()));
            }
            self.emit_line(line);
        }

        self.emit_line(format!(
            "  results: {} ok / {} failed / {} skipped",
            success, failed, skipped
        ));
        self.emit_line(format!("  session data: {}", self.session_dir.display()));
    }

    fn log_job_start(&self, planned: &PlannedJob, ui: Option<&UiBridge>) {
        if let Some(ui) = ui {
            ui.job_started(&planned.job.name);
        }

        if !self.config.enable_tui {
            if self.stage_started(&planned.stage_name) {
                if self.stage_position(&planned.stage_name) > 0 {
                    println!();
                }
                self.emit_line(self.stage_header(&planned.stage_name));
            }

            let job = &planned.job;
            let job_label = self.colorize("  job:", |t| format!("{}", t.bold().green()));
            let job_name = self.colorize(job.name.as_str(), |t| format!("{}", t.bold().white()));
            self.emit_line(format!("{} {}", job_label, job_name));

            if let Some(needs) = Self::format_needs(job) {
                let needs_label = self.colorize("    needs:", |t| format!("{}", t.bold().cyan()));
                self.emit_line(format!("{} {}", needs_label, needs));
            }
            if let Some(paths) = Self::format_paths(&job.artifacts) {
                let artifacts_label =
                    self.colorize("    artifacts:", |t| format!("{}", t.bold().cyan()));
                self.emit_line(format!("{} {}", artifacts_label, paths));
            }

            let job_image = self.resolve_job_image(job);
            let image_label = self.colorize("    image:", |t| format!("{}", t.bold().cyan()));
            self.emit_line(format!("{} {}", image_label, job_image));

            let container_name = self.job_container_name(&planned.stage_name, job);
            let container_label =
                self.colorize("    container:", |t| format!("{}", t.bold().cyan()));
            self.emit_line(format!("{} {}", container_label, container_name));

            if self.verbose_scripts && !job.commands.is_empty() {
                let script_label =
                    self.colorize("    script:", |t| format!("{}", t.bold().yellow()));
                self.emit_line(format!(
                    "{}\n{}",
                    script_label,
                    Self::indent_block(&job.commands.join("\n"), "      │ ")
                ));
            }
        }
    }

    fn run_planned_job(&self, planned: PlannedJob, ui: Option<Arc<UiBridge>>) -> JobEvent {
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
            let mounts = self.build_volume_mounts(&job)?;
            let job_image = self.resolve_job_image(&job);
            let container_name = self.job_container_name(&stage_name, &job);
            let script_commands = self.expanded_commands(&job);
            let script_path = self.write_job_script(&job, &script_commands)?;
            self.execute(
                &script_path,
                &log_path,
                &mounts,
                &job_image,
                &container_name,
                &job,
                ui_ref,
            )?;
            if !self.config.enable_tui {
                self.emit_line(format!("    script stored at {}", script_path.display()));
                self.emit_line(format!("    log file stored at {}", log_path.display()));
                let finish_label =
                    self.colorize("    ✓ finished in", |t| format!("{}", t.bold().green()));
                self.emit_line(format!(
                    "{} {:.2}s",
                    finish_label,
                    job_start.elapsed().as_secs_f32()
                ));

                if let Some(elapsed) = self.stage_job_completed(&stage_name) {
                    let stage_footer = self.colorize("╰─ stage complete in", |t| {
                        format!("{}", t.bold().blue())
                    });
                    self.emit_line(format!("{stage_footer} {:.2}s", elapsed));
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

    fn execute(
        &self,
        script_path: &Path,
        log_path: &Path,
        mounts: &[VolumeMount],
        image: &str,
        container_name: &str,
        job: &Job,
        ui: Option<&UiBridge>,
    ) -> Result<()> {
        if !self.config.enable_tui {
            self.emit_line(self.format_mounts(mounts));
            self.emit_line(self.logs_header());
            let log_label = self.colorize("    log file:", |t| format!("{}", t.bold().yellow()));
            self.emit_line(format!("{} {}", log_label, log_path.display()));
        }

        let container_script = self.container_path_rel(script_path)?;
        if self.verbose_scripts && !self.config.enable_tui {
            let script_label =
                self.colorize("    script file:", |t| format!("{}", t.bold().yellow()));
            self.emit_line(format!("{} {}", script_label, container_script.display()));
        }

        let volume_arg = format!("{}:{}", self.config.workdir.display(), CONTAINER_WORKDIR);

        let mut command = Command::new("container");
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("run")
            .arg("--rm")
            .arg("--arch")
            .arg("x86_64")
            .arg("--name")
            .arg(container_name)
            .arg("--workdir")
            .arg(CONTAINER_WORKDIR)
            .arg("--dns")
            .arg("1.1.1.1")
            .arg("--volume")
            .arg(&volume_arg);

        for mount in mounts {
            command.arg("--volume").arg(mount.to_arg());
        }

        for (key, value) in self.job_env(job) {
            command.arg("--env").arg(format!("{}={}", key, value));
        }

        let mut proc = command
            .arg(image)
            .arg("sh")
            .arg(container_script)
            .spawn()
            .with_context(|| "failed to run command")?;

        let stdout = BufReader::new(proc.stdout.take().unwrap());
        let line_prefix = if self.use_color {
            format!("{}", "    │".dimmed())
        } else {
            "    │".to_string()
        };
        let timestamp_style = |text: &str| {
            if self.use_color {
                format!("{}", text.bold().blue())
            } else {
                text.to_string()
            }
        };
        let line_no_style = |text: &str| {
            if self.use_color {
                format!("{}", text.bold().green())
            } else {
                text.to_string()
            }
        };

        let mut log_file = File::create(log_path)
            .with_context(|| format!("failed to create log at {}", log_path.display()))?;
        let mut log_line_no = 1usize;
        let mut display_line_no = 1usize;
        for line in stdout.lines() {
            let line = line?;
            let timestamp = OffsetDateTime::now_utc()
                .format(TIMESTAMP_FORMAT)
                .unwrap_or_else(|_| "??????????".to_string());
            let fragments = Self::expand_carriage_returns(&line);
            for fragment in &fragments {
                let ts_colored = timestamp_style(&timestamp);
                let no_colored = line_no_style(&format!("{:04}", display_line_no));
                let decorated = format!("[{} {}] {}", ts_colored, no_colored, fragment);
                if !self.config.enable_tui {
                    println!(
                        "{} [{} {}] {}",
                        line_prefix, ts_colored, no_colored, fragment
                    );
                }
                if let Some(ui) = ui {
                    let raw_line = format!("[{} {:04}] {}", timestamp, display_line_no, fragment);
                    ui.job_log_line(&job.name, &raw_line);
                } else {
                    println!("{} {}", line_prefix, decorated);
                }
                display_line_no += 1;
            }
            writeln!(log_file, "[{} {:04}] {}", timestamp, log_line_no, line)?;
            log_line_no += 1;
        }

        let status = proc.wait()?;
        if !status.success() {
            return Err(anyhow!(
                "container command exited with status {:?}",
                status.code()
            ));
        }

        Ok(())
    }

    fn resolve_workdir_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.config.workdir.join(path)
        }
    }

    fn build_volume_mounts(&self, job: &Job) -> Result<Vec<VolumeMount>> {
        let mut mounts = Vec::new();
        for dependency in &job.needs {
            if !dependency.needs_artifacts {
                continue;
            }
            self.append_dependency_mounts(&dependency.job, &mut mounts)?;
        }

        Ok(mounts)
    }

    fn append_dependency_mounts(
        &self,
        job_name: &str,
        mounts: &mut Vec<VolumeMount>,
    ) -> Result<()> {
        let dep_job = self.g.graph.node_weights().find(|job| job.name == job_name);

        let Some(dep_job) = dep_job else {
            warn!(job = job_name, "dependency not present in pipeline graph");
            return Ok(());
        };

        for relative in &dep_job.artifacts {
            let host = self.resolve_workdir_path(relative);
            if !host.exists() {
                warn!(job = job_name, path = %relative.display(), "artifact missing");
                continue;
            }
            let container = self.container_path(relative);
            mounts.push(VolumeMount {
                host,
                container,
                read_only: true,
            });
        }

        Ok(())
    }

    fn container_path(&self, relative: &Path) -> PathBuf {
        if relative.is_absolute() {
            relative.to_path_buf()
        } else {
            Path::new(CONTAINER_WORKDIR).join(relative)
        }
    }

    fn stage_header(&self, name: &str) -> String {
        let prefix = self.colorize("╭────────", |t| {
            format!("{}", t.bold().blue())
        });
        let stage_text = format!("stage {}", name);
        let stage = self.colorize(&stage_text, |t| format!("{}", t.bold().magenta()));
        let suffix = self.colorize("────────╮", |t| {
            format!("{}", t.bold().blue())
        });
        format!("{} {} {}", prefix, stage, suffix)
    }

    fn format_needs(job: &Job) -> Option<String> {
        if job.needs.is_empty() {
            return None;
        }

        let entries: Vec<String> = job
            .needs
            .iter()
            .map(|need| {
                if need.needs_artifacts {
                    format!("{} (artifacts)", need.job)
                } else {
                    need.job.clone()
                }
            })
            .collect();

        Some(entries.join(", "))
    }

    fn format_paths(paths: &[PathBuf]) -> Option<String> {
        if paths.is_empty() {
            return None;
        }

        Some(
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    fn indent_block(block: &str, prefix: &str) -> String {
        let mut buf = String::new();
        for (idx, line) in block.lines().enumerate() {
            if idx > 0 {
                buf.push('\n');
            }
            buf.push_str(prefix);
            buf.push_str(line);
        }
        buf
    }

    fn format_mounts(&self, mounts: &[VolumeMount]) -> String {
        let label = self.colorize("    artifact mounts:", |t| format!("{}", t.bold().cyan()));
        if mounts.is_empty() {
            return format!("{} none", label);
        }

        let desc = mounts
            .iter()
            .map(|mount| mount.container.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");

        format!("{} {}", label, desc)
    }

    fn colorize<'a, F>(&self, text: &'a str, styler: F) -> String
    where
        F: FnOnce(&'a str) -> String,
    {
        if self.use_color {
            styler(text)
        } else {
            text.to_string()
        }
    }

    fn emit_line(&self, line: String) {
        println!("{line}");
    }

    fn logs_header(&self) -> String {
        self.colorize("    logs:", |t| format!("{}", t.bold().cyan()))
    }

    fn resolve_job_image(&self, job: &Job) -> String {
        job.image
            .clone()
            .or_else(|| self.g.defaults.image.clone())
            .unwrap_or_else(|| self.config.image.clone())
    }

    fn job_container_name(&self, stage_name: &str, job: &Job) -> String {
        format!(
            "opal-{}-{}-{}",
            self.run_id,
            stage_name_slug(stage_name),
            job_name_slug(&job.name)
        )
    }

    fn expanded_commands(&self, job: &Job) -> Vec<String> {
        let mut cmds = Vec::new();
        cmds.extend(self.g.defaults.before_script.iter().cloned());
        cmds.extend(job.commands.iter().cloned());
        cmds.extend(self.g.defaults.after_script.iter().cloned());
        cmds
    }

    fn expand_carriage_returns(line: &str) -> Vec<String> {
        let mut parts = Vec::new();
        for fragment in line.split('\r') {
            if fragment.is_empty() {
                continue;
            }
            parts.push(fragment.to_string());
        }
        if parts.is_empty() {
            parts.push(String::new());
        }
        parts
    }

    fn job_env(&self, job: &Job) -> Vec<(String, String)> {
        let mut env = Vec::new();
        let mut push = |key: &str, value: &str| {
            if let Some(existing) = env.iter_mut().find(|(k, _)| k == key) {
                existing.1 = value.to_string();
            } else {
                env.push((key.to_string(), value.to_string()));
            }
        };

        for (key, value) in &self.env_vars {
            push(key, value);
        }
        for (key, value) in &self.g.defaults.variables {
            push(key, value);
        }
        for (key, value) in &job.variables {
            push(key, value);
        }

        env
    }

    fn write_job_script(&self, job: &Job, commands: &[String]) -> Result<PathBuf> {
        let slug = job_name_slug(&job.name);
        let script_path = self.scripts_dir.join(format!("{slug}.sh"));
        if let Some(parent) = script_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create dir {:?}", parent))?;
        }

        let mut file = File::create(&script_path)
            .with_context(|| format!("failed to create script for {}", job.name))?;
        writeln!(file, "#!/usr/bin/env sh")?;
        writeln!(file, "set -eu")?;
        writeln!(file, "cd {}", CONTAINER_WORKDIR)?;
        writeln!(file)?;

        for line in commands {
            if line.trim().is_empty() {
                continue;
            }
            if self.verbose_scripts {
                writeln!(file, "printf '+ %s\\n' \"{}\"", escape_double_quotes(line))?;
            }
            writeln!(file, "{}", line)?;
        }

        Ok(script_path)
    }

    fn job_log_info(&self, job: &Job) -> (PathBuf, String) {
        let mut hasher = Sha256::new();
        hasher.update(self.run_id.as_bytes());
        hasher.update(job.stage.as_bytes());
        hasher.update(job.name.as_bytes());
        let digest = hasher.finalize();
        let hex = format!("{:x}", digest);
        let short = &hex[..12];
        let log_path = self.logs_dir.join(format!("{short}.log"));
        (log_path, short.to_string())
    }

    fn container_path_rel(&self, host_path: &Path) -> Result<PathBuf> {
        let rel = host_path
            .strip_prefix(&self.config.workdir)
            .with_context(|| {
                format!(
                    "path {:?} is outside workspace {:?}",
                    host_path, self.config.workdir
                )
            })?;

        Ok(Path::new(CONTAINER_WORKDIR).join(rel))
    }
}

impl VolumeMount {
    fn to_arg(&self) -> OsString {
        let mut arg = OsString::new();
        arg.push(self.host.as_os_str());
        arg.push(":");
        arg.push(self.container.as_os_str());
        if self.read_only {
            arg.push(":ro");
        }
        arg
    }
}

fn should_use_color() -> bool {
    if env::var_os("NO_COLOR").is_some() {
        return false;
    }

    if env::var_os("CLICOLOR_FORCE").map_or(false, |v| v != "0") {
        return true;
    }

    match env::var("OPAL_COLOR") {
        Ok(val) if matches!(val.as_str(), "always" | "1" | "true") => return true,
        Ok(val) if matches!(val.as_str(), "never" | "0" | "false") => return false,
        _ => {}
    }

    if !io::stdout().is_terminal() {
        return false;
    }

    true
}

fn job_name_slug(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else {
            match ch {
                ' ' | '-' | '_' => slug.push('-'),
                _ => continue,
            }
        }
    }

    if slug.is_empty() {
        slug.push_str("job");
    }

    slug
}

fn stage_name_slug(name: &str) -> String {
    job_name_slug(name)
}

fn collect_env_vars(patterns: &[String]) -> Result<Vec<(String, String)>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob =
            Glob::new(pattern).with_context(|| format!("invalid --env pattern '{pattern}'"))?;
        builder.add(glob);
    }
    let matcher = builder.build()?;

    let vars = env::vars()
        .filter(|(key, _)| matcher.is_match(key))
        .collect();
    Ok(vars)
}

fn generate_run_id(config: &ExecutorConfig) -> String {
    let pipeline_slug = config
        .pipeline
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(job_name_slug)
        .unwrap_or_else(|| "pipeline".to_string());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let mut hasher = Sha256::new();
    hasher.update(pipeline_slug.as_bytes());
    hasher.update(nanos.to_le_bytes());
    hasher.update(process::id().to_le_bytes());

    let digest = hasher.finalize();
    let suffix = format!("{:x}", digest);
    let short = &suffix[..8];
    format!("{pipeline_slug}-{short}")
}

fn escape_double_quotes(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn spawn_job(
    exec: Arc<ContainerExecutor>,
    planned: PlannedJob,
    semaphore: Arc<Semaphore>,
    tx: mpsc::UnboundedSender<JobEvent>,
    ui: Option<Arc<UiBridge>>,
) {
    let job_name = planned.job.name.clone();
    let stage_name = planned.stage_name.clone();
    let log_path = planned.log_path.clone();
    let log_hash = planned.log_hash.clone();
    task::spawn(async move {
        let permit = match semaphore.acquire_owned().await {
            Ok(permit) => permit,
            Err(err) => {
                if let Some(ui) = &ui {
                    ui.job_finished(
                        &job_name,
                        UiJobStatus::Failed,
                        0.0,
                        Some(format!("failed to acquire job slot: {err}")),
                    );
                }
                let _ = tx.send(JobEvent {
                    name: job_name.clone(),
                    stage_name: stage_name.clone(),
                    duration: 0.0,
                    log_path: Some(log_path.clone()),
                    log_hash: log_hash.clone(),
                    result: Err(anyhow!("failed to acquire job slot: {err}")),
                });
                return;
            }
        };

        let exec_clone = exec.clone();
        let planned_job = planned;
        let ui_clone = ui.clone();
        let result =
            task::spawn_blocking(move || exec_clone.run_planned_job(planned_job, ui_clone)).await;
        let event = match result {
            Ok(event) => event,
            Err(err) => JobEvent {
                name: job_name.clone(),
                stage_name: stage_name.clone(),
                duration: 0.0,
                log_path: Some(log_path.clone()),
                log_hash: log_hash.clone(),
                result: Err(anyhow!("job task panicked: {err}")),
            },
        };
        if let Some(ui) = &ui {
            if event.result.is_err() {
                ui.job_finished(
                    &job_name,
                    UiJobStatus::Failed,
                    event.duration,
                    event.result.as_ref().err().map(|e| e.to_string()),
                );
            }
        }

        drop(permit);
        let _ = tx.send(event);
    });
}

#[derive(Debug, Clone)]
struct JobPlan {
    ordered: Vec<String>,
    nodes: HashMap<String, PlannedJob>,
    dependents: HashMap<String, Vec<String>>,
    order_index: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
struct PlannedJob {
    job: Job,
    stage_name: String,
    dependencies: Vec<String>,
    log_path: PathBuf,
    log_hash: String,
}

#[derive(Debug)]
struct JobEvent {
    name: String,
    stage_name: String,
    duration: f32,
    log_path: Option<PathBuf>,
    log_hash: String,
    result: Result<()>,
}

#[derive(Debug, Clone)]
struct JobSummary {
    name: String,
    stage_name: String,
    duration: f32,
    status: JobStatus,
    log_path: Option<PathBuf>,
    log_hash: String,
}

#[derive(Debug, Clone)]
enum JobStatus {
    Success,
    Failed(String),
    Skipped(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HaltKind {
    None,
    JobFailure,
    Deadlock,
    ChannelClosed,
}

#[derive(Debug, Clone)]
struct StageState {
    total: usize,
    completed: usize,
    header_printed: bool,
    started_at: Option<Instant>,
}

impl StageState {
    fn new(total: usize) -> Self {
        Self {
            total,
            completed: 0,
            header_printed: false,
            started_at: None,
        }
    }
}
