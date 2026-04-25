use super::ExecutorCore;
use crate::EngineKind;
use crate::display;
use crate::engine::EngineCommandContext;
use crate::executor::{
    ContainerExecutor, DockerExecutor, NerdctlExecutor, OrbstackExecutor, PodmanExecutor,
    SandboxExecutor,
};
use crate::logging::{self, LogFormatter, sanitize_fragments};
use crate::runner::ExecuteContext;
use crate::ui::UiBridge;
use anyhow::{Context, Result, anyhow};
use owo_colors::OwoColorize;
use std::fs::File;
use std::path::Path;
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::mpsc;
use tracing::debug;

const TIMESTAMP_FORMAT: &[FormatItem<'static>] =
    format_description!("[hour]:[minute]:[second].[subsecond digits:3]");

pub(super) async fn execute(exec: &ExecutorCore, ctx: ExecuteContext<'_>) -> Result<()> {
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
        engine,
        job,
        ui,
        env_vars,
        host_aliases,
        network,
        preserve_runtime_objects,
        arch,
        privileged,
        cap_add,
        cap_drop,
        sandbox_settings,
        sandbox_debug,
    } = ctx;
    if exec.live_console_output_enabled() {
        let display = exec.display();
        display::print_line(display.format_mounts(mounts));
        display::print_line(display.logs_header());
        let log_label = display.bold_yellow("    log file:");
        display::print_line(format!("{} {}", log_label, log_path.display()));
    }

    let container_script = if matches!(engine, EngineKind::Sandbox) {
        script_path.to_path_buf()
    } else {
        exec.container_path_rel(script_path)?
    };
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
        host_aliases,
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

    if let Err(err) = validate_engine_security_options(engine, &command_ctx) {
        let _ = exec.append_job_diagnostics(
            log_path,
            [format!(
                "job container configuration failed before start: {err}"
            )],
        );
        return Err(err);
    }

    let mut proc = spawn_container_process(
        exec,
        engine,
        &command_ctx,
        sandbox_settings,
        sandbox_debug,
        log_path,
    )?;
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
    )
    .await?;

    let status = proc.wait().await?;
    if !status.success() {
        if output_line_count == 0 {
            let _ = exec.append_job_diagnostics(
                log_path,
                [format!(
                    "container command exited with status {:?} before script output (image: {}, script: {})",
                    status.code(),
                    image,
                    container_script.display()
                )],
            );
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
    if matches!(engine, EngineKind::Sandbox)
        && (ctx.privileged || !ctx.cap_add.is_empty() || !ctx.cap_drop.is_empty())
    {
        return Err(anyhow!(
            "the sandbox engine does not support privileged mode or capability flags"
        ));
    }
    Ok(())
}

fn spawn_container_process(
    exec: &ExecutorCore,
    engine: EngineKind,
    ctx: &EngineCommandContext<'_>,
    sandbox_settings: Option<&Path>,
    sandbox_debug: bool,
    log_path: &Path,
) -> Result<Child> {
    let command = match engine {
        EngineKind::ContainerCli => ContainerExecutor::build_command(ctx),
        EngineKind::Docker => DockerExecutor::build_command(ctx),
        EngineKind::Podman => PodmanExecutor::build_command(ctx),
        EngineKind::Nerdctl => NerdctlExecutor::build_command(ctx),
        EngineKind::Orbstack => OrbstackExecutor::build_command(ctx),
        EngineKind::Sandbox => SandboxExecutor::build_command(ctx, sandbox_settings, sandbox_debug),
    };
    let command_line = describe_command(&command);
    debug!(
        engine = ?engine,
        command = %command_line,
        "running job container command"
    );
    let mut tokio_command: tokio::process::Command = command.into();

    match tokio_command.spawn() {
        Ok(child) => Ok(child),
        Err(err) => {
            let _ = exec.append_job_diagnostics(
                log_path,
                [
                    format!("job container engine command: {command_line}"),
                    format!("failed to start job container: {err}"),
                ],
            );
            Err(err)
                .with_context(|| format!("failed to run {:?} command: {}", engine, command_line))
        }
    }
}

fn describe_command(command: &std::process::Command) -> String {
    let program = command.get_program().to_string_lossy();
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if args.is_empty() {
        program.into_owned()
    } else {
        format!("{} {}", program, args.join(" "))
    }
}

async fn capture_output(
    stdout: impl AsyncRead + Unpin + Send + 'static,
    stderr: impl AsyncRead + Unpin + Send + 'static,
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

    let (tx, mut rx) = mpsc::unbounded_channel::<std::result::Result<String, std::io::Error>>();
    tokio::spawn(read_stream_lines(stdout, tx.clone()));
    tokio::spawn(read_stream_lines(stderr, tx.clone()));
    drop(tx);

    while let Some(line) = rx.recv().await {
        let line = line?;
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
    }

    Ok(emitted)
}

async fn read_stream_lines<R>(
    mut reader: R,
    tx: mpsc::UnboundedSender<std::result::Result<String, std::io::Error>>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut chunk = [0u8; 4096];
    let mut buf = Vec::new();
    let mut skip_lf = false;

    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => {
                let _ = emit_fragment(&tx, &mut buf);
                break;
            }
            Ok(read) => {
                for &byte in &chunk[..read] {
                    if skip_lf {
                        skip_lf = false;
                        if byte == b'\n' {
                            continue;
                        }
                    }

                    match byte {
                        b'\r' => {
                            if !emit_fragment(&tx, &mut buf) {
                                return;
                            }
                            skip_lf = true;
                        }
                        b'\n' => {
                            if !emit_fragment(&tx, &mut buf) {
                                return;
                            }
                        }
                        _ => buf.push(byte),
                    }
                }
            }
            Err(err) => {
                let _ = tx.send(Err(err));
                break;
            }
        }
    }
}

fn emit_fragment(
    tx: &mpsc::UnboundedSender<std::result::Result<String, std::io::Error>>,
    buf: &mut Vec<u8>,
) -> bool {
    if buf.is_empty() {
        return true;
    }
    let line = String::from_utf8_lossy(buf).into_owned();
    buf.clear();
    tx.send(Ok(line)).is_ok()
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
    use tokio::runtime::Builder;

    #[test]
    fn capture_output_masks_secret_values_in_log_file() {
        let temp_root = temp_path("process-output-mask");
        let secrets_root = temp_root.join(".opal").join("env");
        fs::create_dir_all(&secrets_root).expect("create secrets dir");
        fs::write(secrets_root.join("API_TOKEN"), "super-secret").expect("write secret");
        let secrets = SecretsStore::load(&temp_root).expect("load secrets");
        let formatter = LogFormatter::new(false).with_secrets(&secrets);
        let log_path = temp_root.join("job.log");

        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let emitted = runtime
            .block_on(capture_output(
                Cursor::new(b"stdout hello\n".to_vec()),
                Cursor::new(b"token=super-secret\n".to_vec()),
                "job",
                &log_path,
                None,
                true,
                &formatter,
            ))
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

        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let emitted = runtime
            .block_on(capture_output(
                Cursor::new(Vec::<u8>::new()),
                Cursor::new(Vec::<u8>::new()),
                "job",
                &log_path,
                None,
                true,
                &formatter,
            ))
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
            host_aliases: &[],
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
