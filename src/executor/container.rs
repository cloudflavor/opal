use super::core::ExecutorCore;
use crate::engine::EngineCommandContext;
use crate::{EngineKind, ExecutorConfig};
use anyhow::Result;
use std::process::{Command, Stdio};

const DEFAULT_MEMORY_LIMIT: &str = "1638m"; // ~1.6 GB
const DEFAULT_CPU_LIMIT: &str = "4";

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
            .with_network()
            .with_env()
            .build()
    }
}

struct ContainerCommandBuilder<'a> {
    ctx: &'a EngineCommandContext<'a>,
    command: Command,
    workspace_mount: String,
}

impl<'a> ContainerCommandBuilder<'a> {
    fn new(ctx: &'a EngineCommandContext<'a>) -> Self {
        let workspace_mount = format!("{}:{}", ctx.workdir.display(), ctx.container_root.display());
        let cpus = ctx.cpus.unwrap_or(DEFAULT_CPU_LIMIT);
        let memory = ctx.memory.unwrap_or(DEFAULT_MEMORY_LIMIT);
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
            .arg("--cpus")
            .arg(cpus)
            .arg("--memory")
            .arg(memory);
        if let Some(dns) = ctx.dns.filter(|value| !value.is_empty()) {
            command.arg("--dns").arg(dns);
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

    fn with_network(mut self) -> Self {
        if let Some(network) = self.ctx.network {
            self.command.arg("--network").arg(network);
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
            .arg("--volume")
            .arg(&self.workspace_mount)
            .arg(self.ctx.image)
            .arg("sh")
            .arg(self.ctx.container_script);
        self.command
    }
}
