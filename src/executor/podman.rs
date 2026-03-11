use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct PDExecutor {
    pub base_image: String,
    pub workdir: PathBuf,
}

impl PDExecutor {
    pub fn new(base_image: String, dir: impl AsRef<Path>) -> Self {
        Self {
            base_image,
            workdir: dir.as_ref().into(),
        }
    }

    pub fn run(&self) -> Result<()> {
        Ok(())
    }
}
