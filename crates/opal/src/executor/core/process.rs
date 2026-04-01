use super::ExecutorCore;
use crate::EngineKind;
use crate::display;
use crate::engine::EngineCommandContext;
use crate::executor::{
    ContainerExecutor, DockerExecutor, NerdctlExecutor, OrbstackExecutor, PodmanExecutor,
};
use crate::logging::{self, LogFormatter, sanitize_fragments};
use crate::runner::ExecuteContext;
use crate::terminal::stream_lines;
use crate::ui::UiBridge;
use anyhow::{Context, Result, anyhow};
use owo_colors::OwoColorize;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Child;
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};

const TIMESTAMP_FORMAT: &[FormatItem<'static>] =
    format_description!("[hour]:[minute]:[second].[subsecond digits:3]");

pub(super) fn execute(exec: &ExecutorCore, ctx: ExecuteContext<'_>) -> Result<()> {
    let ExecuteContext {
        host_workdir,
        script_path,
        log_path,
        mounts,
        image,
        image_platform,
        image_user,
        image_entrypoint,
        container_name,
        job,
        ui,
        env_vars,
        network,
        preserve_runtime_objects,
        arch,
        privileged,
        cap_add,
        cap_drop,
    } = ctx;
    if exec.live_console_output_enabled() {
        let display = exec.display();
        display::print_line(display.format_mounts(mounts));
        display::print_line(display.logs_header());
        let log_label = display.bold_yellow("    log file:");
        display::print_line(format!("{} {}", log_label, log_path.display()));
    }

    let container_script = exec.container_path_rel(script_path)?;
    if exec.verbose_scripts && exec.live_console_output_enabled() {
        let display = exec.display();
        let script_label = display.bold_yellow("    script file:");
        display::print_line(format!("{} {}", script_label, container_script.display()));
    }

    let container_cfg = exec.config.settings.container_settings();
    let command_ctx = EngineCommandContext {
        workdir: host_workdir,
        container_root: &exec.container_workdir,
        container_script: &container_script,
        container_name,
        image,
        image_platform,
        image_user,
        image_entrypoint,
        mounts,
        env_vars,
        network,
        preserve_runtime_objects,
        privileged,
        cap_add,
        cap_drop,
        arch: arch.or_else(|| container_cfg.and_then(|cfg| cfg.arch.as_deref())),
        cpus: container_cfg.and_then(|cfg| cfg.cpus.as_deref()),
        memory: container_cfg.and_then(|cfg| cfg.memory.as_deref()),
        dns: container_cfg.and_then(|cfg| cfg.dns.as_deref()),
    };

    validate_engine_security_options(exec.config.engine, &command_ctx)?;

    let mut proc = spawn_container_process(exec, &command_ctx)?;
    let output_line_count = capture_output(
        proc.stdout
            .take()
            .context("missing stdout from container process")?,
        proc.stderr
            .take()
            .context("missing stderr from container process")?,
        job.name.as_str(),
        log_path,
        ui,
        exec.live_console_output_enabled(),
        &LogFormatter::new(exec.use_color).with_secrets(&exec.secrets),
    )?;

    let status = proc.wait()?;
    if !status.success() {
        if output_line_count == 0 {
            return Err(anyhow!(
                "container command exited with status {:?} before script output; check runtime env keys and container startup (script: {}, image: {})",
                status.code(),
                container_script.display(),
                image
            ));
        }
        return Err(anyhow!(
            "container command exited with status {:?}",
            status.code()
        ));
    }

    Ok(())
}

fn validate_engine_security_options(
    engine: EngineKind,
    ctx: &EngineCommandContext<'_>,
) -> Result<()> {
    if matches!(engine, EngineKind::ContainerCli)
        && (ctx.privileged || !ctx.cap_add.is_empty() || !ctx.cap_drop.is_empty())
    {
        return Err(anyhow!(
            "the Apple 'container' engine does not support privileged mode or capability flags"
        ));
    }
    Ok(())
}

fn spawn_container_process(exec: &ExecutorCore, ctx: &EngineCommandContext<'_>) -> Result<Child> {
    let mut command = match exec.config.engine {
        EngineKind::ContainerCli => ContainerExecutor::build_command(ctx),
        EngineKind::Docker => DockerExecutor::build_command(ctx),
        EngineKind::Podman => PodmanExecutor::build_command(ctx),
        EngineKind::Nerdctl => NerdctlExecutor::build_command(ctx),
        EngineKind::Orbstack => OrbstackExecutor::build_command(ctx),
    };

    command
        .spawn()
        .with_context(|| format!("failed to run {:?} command", exec.config.engine))
}

fn capture_output(
    stdout: impl Read + Send + 'static,
    stderr: impl Read + Send + 'static,
    job_name: &str,
    log_path: &Path,
    ui: Option<&UiBridge>,
    emit_console_output: bool,
    formatter: &LogFormatter<'_>,
) -> Result<usize> {
    let line_prefix = formatter.line_prefix().to_string();
    let mut log_file = File::create(log_path)
        .with_context(|| format!("failed to create log at {}", log_path.display()))?;
    let mut display_line_no = 1usize;
    let mut emitted = 0usize;

    stream_lines(stdout, stderr, |line| {
        let timestamp = OffsetDateTime::now_utc()
            .format(TIMESTAMP_FORMAT)
            .unwrap_or_else(|_| "??????????".to_string());
        for fragment in sanitize_fragments(&line) {
            let masked = formatter.mask(&fragment);
            let plain_line =
                logging::format_plain_log_line(&timestamp, display_line_no, masked.as_ref());
            if let Some(ui) = ui {
                ui.job_log_line(job_name, &plain_line);
            } else if emit_console_output {
                let decorated =
                    formatter.format_masked(&timestamp, display_line_no, masked.as_ref());
                display::print_prefixed_line(
                    &line_prefix,
                    &format_console_stream_line(formatter, job_name, &decorated),
                );
            }
            logging::write_log_line(&mut log_file, &timestamp, display_line_no, masked.as_ref())?;
            display_line_no += 1;
            emitted += 1;
        }
        Ok(())
    })?;

    Ok(emitted)
}

fn format_console_stream_line(
    formatter: &LogFormatter<'_>,
    job_name: &str,
    decorated: &str,
) -> String {
    format!(
        "{} {}",
        format_job_label(job_name, formatter.use_color()),
        decorated
    )
}

fn format_job_label(job_name: &str, use_color: bool) -> String {
    let text = format!("[{}]", job_name);
    if use_color {
        format!("{}", text.bold().magenta())
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::{capture_output, format_console_stream_line, validate_engine_security_options};
    use crate::EngineKind;
    use crate::engine::EngineCommandContext;
    use crate::logging::LogFormatter;
    use crate::secrets::SecretsStore;
    use std::fs;
    use std::io::Cursor;
    use std::path::Path;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn capture_output_masks_secret_values_in_log_file() {
        let temp_root = temp_path("process-output-mask");
        let secrets_root = temp_root.join(".opal").join("env");
        fs::create_dir_all(&secrets_root).expect("create secrets dir");
        fs::write(secrets_root.join("API_TOKEN"), "super-secret").expect("write secret");
        let secrets = SecretsStore::load(&temp_root).expect("load secrets");
        let formatter = LogFormatter::new(false).with_secrets(&secrets);
        let log_path = temp_root.join("job.log");

        let emitted = capture_output(
            Cursor::new(b"stdout hello\n".to_vec()),
            Cursor::new(b"token=super-secret\n".to_vec()),
            "job",
            &log_path,
            None,
            true,
            &formatter,
        )
        .expect("capture output");
        assert_eq!(emitted, 2);

        let contents = fs::read_to_string(&log_path).expect("read log");
        assert!(contents.contains("stdout hello"));
        assert!(contents.contains("token=[MASKED]"));
        assert!(!contents.contains("super-secret"));

        let _ = fs::remove_dir_all(temp_root);
    }

    #[test]
    fn capture_output_reports_zero_when_streams_are_empty() {
        let temp_root = temp_path("process-output-empty");
        fs::create_dir_all(&temp_root).expect("create temp root");
        let formatter = LogFormatter::new(false);
        let log_path = temp_root.join("job.log");

        let emitted = capture_output(
            Cursor::new(Vec::<u8>::new()),
            Cursor::new(Vec::<u8>::new()),
            "job",
            &log_path,
            None,
            true,
            &formatter,
        )
        .expect("capture output");
        assert_eq!(emitted, 0);

        let _ = fs::remove_dir_all(temp_root);
    }

    #[test]
    fn format_console_stream_line_prefixes_job_name() {
        let formatter = LogFormatter::new(false);
        let line = format_console_stream_line(&formatter, "lint", "[12:00:00.000 0001] hello");
        assert!(line.starts_with("[lint] "));
        assert!(line.contains("hello"));
    }

    #[test]
    fn validate_engine_security_options_rejects_container_privileged_mode() {
        let cap_add = vec!["NET_ADMIN".to_string()];
        let ctx = EngineCommandContext {
            workdir: Path::new("/workspace"),
            container_root: Path::new("/builds/workspace"),
            container_script: Path::new("/opal/script.sh"),
            container_name: "opal-job",
            image: "alpine:3.19",
            image_platform: None,
            image_user: None,
            image_entrypoint: &[],
            mounts: &[],
            env_vars: &[],
            network: None,
            preserve_runtime_objects: false,
            arch: None,
            privileged: true,
            cap_add: &cap_add,
            cap_drop: &[],
            cpus: None,
            memory: None,
            dns: None,
        };

        let err = validate_engine_security_options(EngineKind::ContainerCli, &ctx)
            .expect_err("container engine should reject privileged flags");
        assert!(err.to_string().contains("does not support privileged mode"));
    }

    fn temp_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("opal-{prefix}-{nanos}"))
    }
}
