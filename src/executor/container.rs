use crate::ExecutorConfig;
use crate::pipeline::{Job, PipelineGraph, StageGroup};
use anyhow::{Context, Result, anyhow};
use globset::{Glob, GlobSetBuilder};
use owo_colors::OwoColorize;
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use time::format_description::FormatItem;
use time::{OffsetDateTime, macros::format_description};
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
    run_id: String,
    verbose_scripts: bool,
    env_vars: Vec<(String, String)>,
}

impl ContainerExecutor {
    pub fn new(config: ExecutorConfig) -> Result<Self> {
        let g = PipelineGraph::from_path(&config.pipeline)?;
        let scripts_dir = config.workdir.join(".opal").join("scripts");
        if scripts_dir.exists() {
            fs::remove_dir_all(&scripts_dir)
                .with_context(|| format!("failed to clean {:?}", scripts_dir))?;
        }
        fs::create_dir_all(&scripts_dir)
            .with_context(|| format!("failed to create {:?}", scripts_dir))?;

        let use_color = should_use_color();
        let run_id = generate_run_id(&config);
        let verbose_scripts = env::var_os("OPAL_DEBUG")
            .map(|val| {
                let s = val.to_string_lossy();
                s == "1" || s.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false);
        let env_vars = collect_env_vars(&config.env_includes)?;

        Ok(Self {
            config,
            g,
            use_color,
            scripts_dir,
            run_id,
            verbose_scripts,
            env_vars,
        })
    }

    pub fn run(&self) -> Result<()> {
        for (idx, stage) in self.g.stages.iter().enumerate() {
            let stage_start = Instant::now();
            if idx > 0 {
                println!("");
            }
            self.emit_line(self.stage_header(&stage.name));
            for node_idx in &stage.jobs {
                if let Some(job) = self.g.graph.node_weight(*node_idx) {
                    let mounts = self.build_volume_mounts(job)?;
                    let job_label = self.colorize("  job:", |t| format!("{}", t.bold().green()));
                    let job_name =
                        self.colorize(job.name.as_str(), |t| format!("{}", t.bold().white()));
                    self.emit_line(format!("{} {}", job_label, job_name));
                    if let Some(needs) = Self::format_needs(job) {
                        let needs_label =
                            self.colorize("    needs:", |t| format!("{}", t.bold().cyan()));
                        self.emit_line(format!("{} {}", needs_label, needs));
                    }
                    if let Some(paths) = Self::format_paths(&job.artifacts) {
                        let artifacts_label =
                            self.colorize("    artifacts:", |t| format!("{}", t.bold().cyan()));
                        self.emit_line(format!("{} {}", artifacts_label, paths));
                    }

                    let job_image = self.resolve_job_image(job);
                    let image_label =
                        self.colorize("    image:", |t| format!("{}", t.bold().cyan()));
                    self.emit_line(format!("{} {}", image_label, job_image));
                    let container_name = self.job_container_name(stage, job);
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

                    let job_start = Instant::now();
                    let script_commands = self.expanded_commands(job);
                    let script_path = self.write_job_script(job, &script_commands)?;
                    self.execute(&script_path, &mounts, &job_image, &container_name, job)?;
                    self.emit_line(format!("    script stored at {}", script_path.display()));
                    let finish_label =
                        self.colorize("    ✓ finished in", |t| format!("{}", t.bold().green()));
                    self.emit_line(format!(
                        "{} {:.2}s",
                        finish_label,
                        job_start.elapsed().as_secs_f32()
                    ));
                }
            }

            let stage_footer = self.colorize("╰─ stage complete in", |t| {
                format!("{}", t.bold().blue())
            });
            self.emit_line(format!(
                "{} {:.2}s",
                stage_footer,
                stage_start.elapsed().as_secs_f32()
            ));
        }

        Ok(())
    }

    fn execute(
        &self,
        script_path: &Path,
        mounts: &[VolumeMount],
        image: &str,
        container_name: &str,
        job: &Job,
    ) -> Result<()> {
        self.emit_line(self.format_mounts(mounts));
        self.emit_line(self.logs_header());

        let container_script = self.container_path_rel(script_path)?;
        if self.verbose_scripts {
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

        let mut line_no: usize = 1;
        for line in stdout.lines() {
            let line = line?;
            let timestamp = OffsetDateTime::now_utc()
                .format(TIMESTAMP_FORMAT)
                .unwrap_or_else(|_| "??????????".to_string());
            let ts_colored = timestamp_style(&timestamp);
            let no_colored = line_no_style(&format!("{:04}", line_no));
            println!("{} [{} {}] {}", line_prefix, ts_colored, no_colored, line);
            line_no += 1;
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

    fn job_container_name(&self, stage: &StageGroup, job: &Job) -> String {
        format!(
            "opal-{}-{}-{}",
            self.run_id,
            stage_name_slug(&stage.name),
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
