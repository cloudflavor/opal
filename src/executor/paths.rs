use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::executor::core::CONTAINER_WORKDIR;

pub fn to_container_path(host_path: &Path, workdir: &Path) -> Result<PathBuf> {
    let rel = host_path
        .strip_prefix(workdir)
        .with_context(|| format!("path {:?} is outside workspace {:?}", host_path, workdir))?;
    Ok(Path::new(CONTAINER_WORKDIR).join(rel))
}
