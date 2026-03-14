use super::core::ExecutorCore;
use crate::{EngineKind, ExecutorConfig};
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct DockerExecutor {
    core: ExecutorCore,
}

impl DockerExecutor {
    pub fn new(mut config: ExecutorConfig) -> Result<Self> {
        config.engine = EngineKind::Docker;
        let core = ExecutorCore::new(config)?;
        Ok(Self { core })
    }

    pub async fn run(&self) -> Result<()> {
        self.core.run().await
    }
}
