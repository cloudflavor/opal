use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ResourceGroupManager {
    root: PathBuf,
}

impl ResourceGroupManager {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn try_acquire(&self, group: &str, owner: &str) -> Result<bool> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("failed to create {}", self.root.display()))?;
        let path = self.lock_path(group);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                writeln!(file, "{owner}")
                    .with_context(|| format!("failed to write {}", path.display()))?;
                Ok(true)
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(err) => Err(err).with_context(|| format!("failed to open {}", path.display())),
        }
    }

    pub fn release(&self, group: &str) -> Result<()> {
        let path = self.lock_path(group);
        if !path.exists() {
            return Ok(());
        }
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))
    }

    fn lock_path(&self, group: &str) -> PathBuf {
        self.root
            .join(format!("{}.lock", sanitize_group_name(group)))
    }
}

fn sanitize_group_name(group: &str) -> String {
    let mut slug = String::new();
    for ch in group.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | '.') {
            slug.push('-');
        }
    }
    if slug.is_empty() {
        slug.push_str("resource-group");
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::ResourceGroupManager;
    use tempfile::tempdir;

    #[test]
    fn resource_group_manager_blocks_second_acquire() {
        let dir = tempdir().expect("tempdir");
        let manager = ResourceGroupManager::new(dir.path().join("locks"));

        assert!(manager.try_acquire("prod-lock", "job-a").unwrap());
        assert!(!manager.try_acquire("prod-lock", "job-b").unwrap());
        manager.release("prod-lock").unwrap();
        assert!(manager.try_acquire("prod-lock", "job-b").unwrap());
    }
}
