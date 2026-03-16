use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

pub fn to_container_path(host_path: &Path, mappings: &[(&Path, &Path)]) -> Result<PathBuf> {
    for (host_base, container_base) in mappings {
        if let Ok(rel) = host_path.strip_prefix(host_base) {
            return Ok(container_base.join(rel));
        }
    }
    let mut bases = String::new();
    for (host_base, _) in mappings {
        bases.push_str(&format!("{}\n", host_base.display()));
    }
    Err(anyhow!(
        "path {:?} is outside allowed roots:\n{}",
        host_path,
        bases.trim_end()
    ))
}
