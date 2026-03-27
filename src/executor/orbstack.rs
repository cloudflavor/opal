use super::core::ExecutorCore;
use crate::engine::EngineCommandContext;
use crate::{EngineKind, ExecutorConfig};
use anyhow::Result;
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct OrbstackExecutor {
    core: ExecutorCore,
}

impl OrbstackExecutor {
    pub fn new(mut config: ExecutorConfig) -> Result<Self> {
        config.engine = EngineKind::Orbstack;
        let core = ExecutorCore::new(config)?;
        Ok(Self { core })
    }

    pub async fn run(&self) -> Result<()> {
        self.core.run().await
    }

    pub fn build_command(ctx: &EngineCommandContext<'_>) -> Command {
        OrbstackCommandBuilder::new(ctx)
            .with_workspace_volume()
            .with_volumes()
            .with_network()
            .with_platform()
            .with_image_options()
            .with_privileges()
            .with_env()
            .build()
    }
}

struct OrbstackCommandBuilder<'a> {
    ctx: &'a EngineCommandContext<'a>,
    command: Command,
    workspace_mount: String,
}

impl<'a> OrbstackCommandBuilder<'a> {
    fn new(ctx: &'a EngineCommandContext<'a>) -> Self {
        let workspace_mount = format!("{}:{}", ctx.workdir.display(), ctx.container_root.display());
        let mut command = Command::new("docker");
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("run")
            .arg("--name")
            .arg(ctx.container_name)
            .arg("--workdir")
            .arg(ctx.container_root);
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
    use super::OrbstackExecutor;
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
            network: None,
            arch: None,
            privileged: false,
            cap_add: &[],
            cap_drop: &[],
            cpus: None,
            memory: None,
            dns: None,
        };

        let args: Vec<String> = OrbstackExecutor::build_command(&ctx)
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert!(
            args.windows(2)
                .any(|pair| pair == ["--platform", "linux/arm64/v8"])
        );
    }
}
