use super::core::ExecutorCore;
use crate::{EngineKind, ExecutorConfig};
use anyhow::Result;

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
}
