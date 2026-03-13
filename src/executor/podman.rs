use crate::ExecutorConfig;
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct PDExecutor {
    pub config: ExecutorConfig,
}

impl PDExecutor {
    pub fn new(config: ExecutorConfig) -> Self {
        Self { config }
    }

    pub fn run(&self) -> Result<()> {
        Ok(())
    }
}
