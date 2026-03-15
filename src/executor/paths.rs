use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn to_container_path(
    host_path: &Path,
    workdir: &Path,
    container_workdir: &Path,
) -> Result<PathBuf> {
    let rel = host_path
        .strip_prefix(workdir)
        .with_context(|| format!("path {:?} is outside workspace {:?}", host_path, workdir))?;
    Ok(container_workdir.join(rel))
}
