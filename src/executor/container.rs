use super::core::ExecutorCore;
use crate::engine::EngineCommandContext;
use crate::{EngineKind, ExecutorConfig};
use anyhow::Result;
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct ContainerExecutor {
    core: ExecutorCore,
}

impl ContainerExecutor {
    pub fn new(mut config: ExecutorConfig) -> Result<Self> {
        config.engine = EngineKind::ContainerCli;
        let core = ExecutorCore::new(config)?;
        Ok(Self { core })
    }

    pub async fn run(&self) -> Result<()> {
        self.core.run().await
    }

    pub fn build_command(ctx: &EngineCommandContext<'_>) -> Command {
        ContainerCommandBuilder::new(ctx)
            .with_volumes()
            .with_env()
            .build()
    }
}

struct ContainerCommandBuilder<'a> {
    ctx: &'a EngineCommandContext<'a>,
    command: Command,
}

impl<'a> ContainerCommandBuilder<'a> {
    fn new(ctx: &'a EngineCommandContext<'a>) -> Self {
        let workspace_mount = format!("{}:{}", ctx.workdir.display(), ctx.container_root.display());
        let mut command = Command::new("container");
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("run")
            .arg("--arch")
            .arg("x86_64")
            .arg("--name")
            .arg(ctx.container_name)
            .arg("--workdir")
            .arg(ctx.container_root)
            .arg("--dns")
            .arg("1.1.1.1")
            .arg("--volume")
            .arg(&workspace_mount);
        Self { ctx, command }
    }

    fn with_volumes(mut self) -> Self {
        for mount in self.ctx.mounts {
            self.command.arg("--volume").arg(mount.to_arg());
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
