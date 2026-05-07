use std::process::Stdio;

use super::ExecutorCore;
use crate::EngineKind;
use tokio::process::Command;
use tracing::warn;

pub(super) async fn kill_container(
    _exec: &ExecutorCore,
    engine: EngineKind,
    job_name: &str,
    container_name: &str,
) -> Option<String> {
    let mut command = force_remove_container_command(engine, container_name);
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    match command.output().await {
        Ok(output) => stderr_message(&output.stderr),
        Err(err) => {
            warn!(
                job = job_name,
                container = container_name,
                error = %err,
                "failed to terminate container after timeout"
            );
            Some(format!(
                "failed to remove container {container_name}: {err}"
            ))
        }
    }
}

pub(super) async fn cleanup_finished_container(
    _exec: &ExecutorCore,
    engine: EngineKind,
    container_name: &str,
) -> Option<String> {
    let mut command = force_remove_container_command(engine, container_name);
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    match command.output().await {
        Ok(output) => stderr_message(&output.stderr),
        Err(err) => Some(format!(
            "failed to remove container {container_name}: {err}"
        )),
    }
}

fn stderr_message(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn force_remove_container_command(engine: EngineKind, container_name: &str) -> Command {
    let mut command = Command::new(container_binary(engine));
    let [subcommand, force_flag] = force_remove_args(engine);
    command.arg(subcommand).arg(force_flag).arg(container_name);
    command
}

fn container_binary(engine: EngineKind) -> &'static str {
    match engine {
        EngineKind::ContainerCli => "container",
        EngineKind::Docker | EngineKind::Orbstack => "docker",
        EngineKind::Podman => "podman",
        EngineKind::Nerdctl => "nerdctl",
        EngineKind::Sandbox => "srt",
    }
}

fn force_remove_args(engine: EngineKind) -> [&'static str; 2] {
    match engine {
        EngineKind::ContainerCli => ["rm", "--force"],
        EngineKind::Docker
        | EngineKind::Orbstack
        | EngineKind::Podman
        | EngineKind::Nerdctl
        | EngineKind::Sandbox => ["rm", "-f"],
    }
}

#[cfg(test)]
mod tests {
    use super::{container_binary, force_remove_args};
    use crate::EngineKind;

    #[test]
    fn force_remove_args_match_engine_cli() {
        assert_eq!(
            force_remove_args(EngineKind::ContainerCli),
            ["rm", "--force"]
        );
        assert_eq!(force_remove_args(EngineKind::Docker), ["rm", "-f"]);
        assert_eq!(force_remove_args(EngineKind::Orbstack), ["rm", "-f"]);
        assert_eq!(force_remove_args(EngineKind::Podman), ["rm", "-f"]);
        assert_eq!(force_remove_args(EngineKind::Nerdctl), ["rm", "-f"]);
        assert_eq!(force_remove_args(EngineKind::Sandbox), ["rm", "-f"]);
    }

    #[test]
    fn container_binary_matches_engine_family() {
        assert_eq!(container_binary(EngineKind::ContainerCli), "container");
        assert_eq!(container_binary(EngineKind::Docker), "docker");
        assert_eq!(container_binary(EngineKind::Orbstack), "docker");
        assert_eq!(container_binary(EngineKind::Podman), "podman");
        assert_eq!(container_binary(EngineKind::Nerdctl), "nerdctl");
        assert_eq!(container_binary(EngineKind::Sandbox), "srt");
    }
}
