use super::core::ExecutorCore;
use crate::{EngineKind, ExecutorConfig};
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct NerdctlExecutor {
    core: ExecutorCore,
}

impl NerdctlExecutor {
    pub fn new(mut config: ExecutorConfig) -> Result<Self> {
        config.engine = EngineKind::Nerdctl;
        let core = ExecutorCore::new(config)?;
        Ok(Self { core })
    }

    pub async fn run(&self) -> Result<()> {
        self.core.run().await
    }
}
