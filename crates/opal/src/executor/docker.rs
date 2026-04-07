use super::container_engine::build_container_engine_command;
use super::core::{ExecutionOutcome, ExecutorCore};
use crate::engine::EngineCommandContext;
use crate::{EngineKind, ExecutorConfig};
use anyhow::Result;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct DockerExecutor {
    core: ExecutorCore,
}

impl DockerExecutor {
    pub async fn new(mut config: ExecutorConfig) -> Result<Self> {
        config.engine = EngineKind::Docker;
        let core = ExecutorCore::new(config).await?;
        Ok(Self { core })
    }

    pub async fn run(&self) -> ExecutionOutcome {
        self.core.run().await
    }

    pub fn build_command(ctx: &EngineCommandContext<'_>) -> Command {
        build_container_engine_command("docker", ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::DockerExecutor;
    use crate::engine::EngineCommandContext;
    use std::path::Path;

    #[test]
    fn build_command_uses_docker_binary() {
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
            privileged: false,
            cap_add: &[],
            cap_drop: &[],
            cpus: None,
            memory: None,
            dns: None,
        };

        let command = DockerExecutor::build_command(&ctx);

        assert_eq!(command.get_program().to_string_lossy(), "docker");
    }
}
