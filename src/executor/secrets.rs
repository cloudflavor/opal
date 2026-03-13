use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const SECRETS_RELATIVE_DIR: &str = ".opal/env";
pub const SECRETS_CONTAINER_DIR: &str = "/workspace/.opal/secrets";

#[derive(Debug, Default, Clone)]
pub struct SecretsStore {
    root: Option<PathBuf>,
    entries: Vec<SecretEntry>,
}

#[derive(Debug, Clone)]
struct SecretEntry {
    name: String,
    rel_path: PathBuf,
    value: Option<String>,
}

impl SecretsStore {
    pub fn load(workdir: &Path) -> Result<Self> {
        let dir = workdir.join(SECRETS_RELATIVE_DIR);
        if !dir.exists() || !dir.is_dir() {
            return Ok(Self::default());
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read secrets directory at {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                let bytes = fs::read(&path)
                    .with_context(|| format!("failed to read secret {}", path.display()))?;
                let value = String::from_utf8(bytes.clone()).ok();
                entries.push(SecretEntry {
                    name: name.to_string(),
                    rel_path: PathBuf::from(name),
                    value,
                });
            }
        }

        Ok(Self {
            root: Some(dir),
            entries,
        })
    }

    pub fn has_secrets(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn extend_env(&self, env: &mut Vec<(String, String)>) {
        for entry in &self.entries {
            if let Some(value) = &entry.value {
                env.push((entry.name.clone(), value.clone()));
            }
            let file_env = format!("{}_FILE", entry.name);
            let file_path = Path::new(SECRETS_CONTAINER_DIR).join(&entry.rel_path);
            env.push((file_env, file_path.display().to_string()));
        }
    }

    pub fn mask_fragment<'a>(&self, fragment: &'a str) -> Cow<'a, str> {
        if self.entries.is_empty() {
            return Cow::Borrowed(fragment);
        }
        let mut masked = Cow::Borrowed(fragment);
        for entry in &self.entries {
            if let Some(value) = &entry.value {
                if value.is_empty() {
                    continue;
                }
                if let Cow::Borrowed(current) = &masked
                    && !current.contains(value)
                {
                    continue;
                }
                let replaced = masked.replace(value, "[MASKED]");
                masked = Cow::Owned(replaced);
            }
        }
        masked
    }

    pub fn volume_mount(&self) -> Option<(PathBuf, PathBuf)> {
        let root = self.root.as_ref()?;
        Some((root.clone(), PathBuf::from(SECRETS_CONTAINER_DIR)))
    }
}
