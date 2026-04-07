use crate::EngineKind;
use crate::executor::container_arch::default_container_cli_arch;
use crate::model::ServiceSpec;
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::env;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const CONTAINER_COMMAND_TIMEOUT_DEFAULT_SECS: u64 = 10;

pub(super) fn engine_binary(engine: EngineKind) -> &'static str {
    match engine {
        EngineKind::Docker | EngineKind::Orbstack => "docker",
        EngineKind::Podman => "podman",
        EngineKind::Nerdctl => "nerdctl",
        EngineKind::ContainerCli => "container",
    }
}

pub(super) fn service_command(engine: EngineKind, service: &ServiceSpec) -> Command {
    let mut command = Command::new(engine_binary(engine));
    command.arg("run");
    if matches!(engine, EngineKind::ContainerCli) {
        if let Some(arch) = default_container_cli_arch(service.docker_platform.as_deref()) {
            command.arg("--arch").arg(arch);
        }
    } else if let Some(platform) = &service.docker_platform {
        command.arg("--platform").arg(platform);
    }
    command
}

pub(super) fn force_remove_container_command(engine: EngineKind, container_name: &str) -> Command {
    let mut command = Command::new(engine_binary(engine));
    let [subcommand, force_flag] = force_remove_args(engine);
    command.arg(subcommand).arg(force_flag).arg(container_name);
    command
}

pub(super) fn merged_env(
    base: &[(String, String)],
    overrides: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let lookup: HashMap<String, String> = base.iter().cloned().collect();
    let mut env = base.to_vec();
    for (_, value) in &mut env {
        *value = crate::env::expand_value(value, &lookup);
    }

    let mut merged: HashMap<String, String> = env.into_iter().collect();
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    merged.into_iter().collect()
}

pub(super) fn run_command_with_timeout(mut cmd: Command, timeout: Option<Duration>) -> Result<()> {
    let Some(timeout) = timeout else {
        return run_command(cmd);
    };

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let started = Instant::now();

    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            if output.status.success() {
                return Ok(());
            }
            return Err(command_failed(
                &cmd,
                &output.stdout,
                &output.stderr,
                output.status.code(),
            ));
        }

        if started.elapsed() >= timeout {
            let _ = child.kill();
            let output = child.wait_with_output().ok();
            let (stdout, stderr, _) = if let Some(output) = output {
                (output.stdout, output.stderr, output.status.code())
            } else {
                (Vec::new(), Vec::new(), None)
            };
            return Err(anyhow!(
                "command {:?} timed out after {}s{}",
                &cmd,
                timeout.as_secs(),
                command_failed_detail(&stdout, &stderr)
            ));
        }

        thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn command_timeout(engine: EngineKind) -> Option<Duration> {
    if matches!(engine, EngineKind::ContainerCli) {
        Some(container_command_timeout())
    } else {
        None
    }
}

pub(super) fn command_failed(
    cmd: &Command,
    stdout: &[u8],
    stderr: &[u8],
    code: Option<i32>,
) -> anyhow::Error {
    anyhow!(
        "command {:?} exited with status {:?}{}",
        cmd,
        code,
        command_failed_detail(stdout, stderr)
    )
}

fn run_command(mut cmd: Command) -> Result<()> {
    let output = cmd.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failed(
            &cmd,
            &output.stdout,
            &output.stderr,
            output.status.code(),
        ))
    }
}

fn container_command_timeout() -> Duration {
    env::var("OPAL_CONTAINER_COMMAND_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(CONTAINER_COMMAND_TIMEOUT_DEFAULT_SECS))
}

fn command_failed_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(": {stdout}"),
        (true, false) => format!(": {stderr}"),
        (false, false) => format!(": stdout={stdout}; stderr={stderr}"),
    }
}

fn force_remove_args(engine: EngineKind) -> [&'static str; 2] {
    match engine {
        EngineKind::ContainerCli => ["rm", "--force"],
        EngineKind::Docker | EngineKind::Orbstack | EngineKind::Podman | EngineKind::Nerdctl => {
            ["rm", "-f"]
        }
    }
}
