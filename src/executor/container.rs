use super::core::ExecutorCore;
use crate::engine::EngineCommandContext;
use crate::{EngineKind, ExecutorConfig};
use anyhow::Result;
use std::env;
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
            .arg("--rm")
            .arg("--name")
            .arg(ctx.container_name)
            .arg("--workdir")
            .arg(ctx.container_root)
            .arg("--cpus")
            .arg(cpus)
            .arg("--memory")
            .arg(memory);
        if let Some(arch) = container_arch_override().or_else(host_container_arch) {
            command.arg("--arch").arg(arch);
        }
        if let Some(dns) = ctx
            .dns
            .filter(|value| !value.is_empty())
            .or(Some("1.1.1.1"))
        {
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
        if env::var("OPAL_DEBUG_CONTAINER")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            let program = self.command.get_program().to_string_lossy();
            let args: Vec<String> = self
                .command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect();
            eprintln!("[opal] container command: {} {}", program, args.join(" "));
        }
        self.command
    }
}

fn container_arch_override() -> Option<String> {
    env::var("OPAL_CONTAINER_ARCH")
        .ok()
        .filter(|value| !value.is_empty())
}

fn host_container_arch() -> Option<String> {
    // Apple's container CLI currently expects x86_64 guests even on Apple silicon hosts.
    // Default to x86_64 unless OPAL_CONTAINER_ARCH overrides it.
    Some("x86_64".to_string())
}

#[cfg(test)]
mod tests {
    use super::ContainerExecutor;
    use crate::engine::EngineCommandContext;
    use std::path::Path;

    #[test]
    fn build_command_uses_rm_for_job_containers() {
        let ctx = EngineCommandContext {
            workdir: Path::new("/workspace"),
            container_root: Path::new("/builds/workspace"),
            container_script: Path::new("/opal/script.sh"),
            container_name: "opal-job",
            image: "alpine:3.19",
            mounts: &[],
            env_vars: &[],
            network: None,
            cpus: None,
            memory: None,
            dns: None,
        };

        let command = ContainerExecutor::build_command(&ctx);
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert!(args.iter().any(|arg| arg == "--rm"));
    }
}
