use super::core::{ExecutionOutcome, ExecutorCore};
use crate::engine::EngineCommandContext;
use crate::{EngineKind, ExecutorConfig};
use anyhow::Result;
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct PodmanExecutor {
    core: ExecutorCore,
}

impl PodmanExecutor {
    pub async fn new(mut config: ExecutorConfig) -> Result<Self> {
        config.engine = EngineKind::Podman;
        let core = ExecutorCore::new(config).await?;
        Ok(Self { core })
    }

    pub async fn run(&self) -> ExecutionOutcome {
        self.core.run().await
    }

    pub async fn run_with_progress(
        &self,
        progress: Option<super::core::ExecutionProgressCallback>,
    ) -> ExecutionOutcome {
        self.core.run_with_progress(progress).await
    }

    pub(crate) async fn run_with_progress_and_commands(
        &self,
        progress: Option<super::core::ExecutionProgressCallback>,
        commands: Option<tokio::sync::mpsc::UnboundedReceiver<crate::ui::UiCommand>>,
    ) -> ExecutionOutcome {
        self.core
            .run_with_progress_and_commands(progress, commands)
            .await
    }

    pub fn build_command(ctx: &EngineCommandContext<'_>) -> Command {
        PodmanCommandBuilder::new(ctx, nested_podman_run())
            .with_workspace_volume()
            .with_volumes()
            .with_network()
            .with_platform()
            .with_image_options()
            .with_privileges()
            .with_host_aliases()
            .with_env()
            .build()
    }
}

fn nested_podman_run() -> bool {
    std::env::var("OPAL_IN_OPAL").is_ok_and(|value| value == "1")
}

struct PodmanCommandBuilder<'a> {
    ctx: &'a EngineCommandContext<'a>,
    command: Command,
    workspace_mount: String,
}

impl<'a> PodmanCommandBuilder<'a> {
    fn new(ctx: &'a EngineCommandContext<'a>, disable_cgroups: bool) -> Self {
        let workspace_mount = format!("{}:{}", ctx.workdir.display(), ctx.container_root.display());
        let mut command = Command::new("podman");
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("run")
            .arg("--name")
            .arg(ctx.container_name)
            .arg("--workdir")
            .arg(ctx.container_root);
        if disable_cgroups {
            command.arg("--cgroups=disabled");
        }
        Self {
            ctx,
            command,
            workspace_mount,
        }
    }

    fn with_volumes(mut self) -> Self {
        for mount in self.ctx.mounts {
            self.command.arg("--volume").arg(mount.to_arg());
        }
        self
    }

    fn with_workspace_volume(mut self) -> Self {
        self.command.arg("--volume").arg(&self.workspace_mount);
        self
    }

    fn with_network(mut self) -> Self {
        if let Some(network) = self.ctx.network {
            self.command.arg("--network").arg(network);
        }
        self
    }

    fn with_platform(mut self) -> Self {
        if let Some(platform) = self.ctx.image_platform {
            self.command.arg("--platform").arg(platform);
        }
        self
    }

    fn with_image_options(mut self) -> Self {
        if let Some(user) = self.ctx.image_user {
            self.command.arg("--user").arg(user);
        }
        if !self.ctx.image_entrypoint.is_empty() {
            self.command
                .arg("--entrypoint")
                .arg(self.ctx.image_entrypoint.join(" "));
        }
        self
    }

    fn with_privileges(mut self) -> Self {
        if self.ctx.privileged {
            self.command.arg("--privileged");
        }
        for capability in self.ctx.cap_add {
            self.command.arg("--cap-add").arg(capability);
        }
        for capability in self.ctx.cap_drop {
            self.command.arg("--cap-drop").arg(capability);
        }
        self
    }

    fn with_host_aliases(mut self) -> Self {
        for (host, ip) in self.ctx.host_aliases {
            self.command.arg("--add-host").arg(format!("{host}:{ip}"));
        }
        self
    }

    fn with_env(mut self) -> Self {
        for (key, value) in self.ctx.env_vars {
            self.command.arg("--env").arg(format!("{key}={value}"));
        }
        self
    }

    fn build(mut self) -> Command {
        self.command
            .arg(self.ctx.image)
            .arg("sh")
            .arg(self.ctx.container_script);
        self.command
    }
}

#[cfg(test)]
mod tests {
    use super::{PodmanCommandBuilder, PodmanExecutor};
    use crate::engine::EngineCommandContext;
    use std::path::Path;

    #[test]
    fn build_command_includes_platform_when_requested() {
        let ctx = EngineCommandContext {
            workdir: Path::new("/workspace"),
            container_root: Path::new("/builds/workspace"),
            container_script: Path::new("/opal/script.sh"),
            container_name: "opal-job",
            image: "alpine:3.19",
            image_platform: Some("linux/arm64/v8"),
            image_user: None,
            image_entrypoint: &[],
            mounts: &[],
            env_vars: &[],
            host_aliases: &[],
            network: None,
            preserve_runtime_objects: false,
            arch: None,
            privileged: false,
            cap_add: &[],
            cap_drop: &[],
            cpus: None,
            memory: None,
            dns: None,
        };

        let args: Vec<String> = PodmanExecutor::build_command(&ctx)
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert!(
            args.windows(2)
                .any(|pair| pair == ["--platform", "linux/arm64/v8"])
        );
    }

    #[test]
    fn build_command_disables_cgroups_for_nested_runs() {
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
            privileged: false,
            cap_add: &[],
            cap_drop: &[],
            cpus: None,
            memory: None,
            dns: None,
        };

        let args: Vec<String> = PodmanCommandBuilder::new(&ctx, true)
            .with_workspace_volume()
            .with_volumes()
            .with_network()
            .with_platform()
            .with_image_options()
            .with_privileges()
            .with_host_aliases()
            .with_env()
            .build()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert!(args.iter().any(|arg| arg == "--cgroups=disabled"));
    }
}
