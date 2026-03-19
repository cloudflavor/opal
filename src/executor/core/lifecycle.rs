use super::ExecutorCore;
use crate::EngineKind;
use std::process::{Command, Stdio};
use tracing::warn;

pub(super) fn kill_container(exec: &ExecutorCore, job_name: &str, container_name: &str) {
    let mut command = force_remove_container_command(exec.config.engine, container_name);
    if let Err(err) = command.status() {
        warn!(
            job = job_name,
            container = container_name,
            error = %err,
            "failed to terminate container after timeout"
        );
    }
}

pub(super) fn cleanup_finished_container(exec: &ExecutorCore, container_name: &str) {
    let mut command = force_remove_container_command(exec.config.engine, container_name);
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let _ = command.status();
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
    }

    #[test]
    fn container_binary_matches_engine_family() {
        assert_eq!(container_binary(EngineKind::ContainerCli), "container");
        assert_eq!(container_binary(EngineKind::Docker), "docker");
        assert_eq!(container_binary(EngineKind::Orbstack), "docker");
        assert_eq!(container_binary(EngineKind::Podman), "podman");
        assert_eq!(container_binary(EngineKind::Nerdctl), "nerdctl");
    }
}
