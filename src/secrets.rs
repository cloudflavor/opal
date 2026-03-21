use anyhow::{Context, Result};
use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};

const SECRETS_RELATIVE_DIR: &str = ".opal/env";
const LEGACY_SECRETS_RELATIVE_DIR: &str = ".opal";
pub const SECRETS_CONTAINER_DIR: &str = "/opal/secrets";

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
        let scoped_dir = workdir.join(SECRETS_RELATIVE_DIR);
        if scoped_dir.exists() && scoped_dir.is_dir() {
            return Ok(Self {
                root: Some(scoped_dir.clone()),
                entries: load_secret_entries(&scoped_dir, false)?,
            });
        }

        let legacy_dir = workdir.join(LEGACY_SECRETS_RELATIVE_DIR);
        if legacy_dir.exists() && legacy_dir.is_dir() {
            let entries = load_secret_entries(&legacy_dir, true)?;
            if !entries.is_empty() {
                return Ok(Self {
                    root: Some(legacy_dir),
                    entries,
                });
            }
        }

        Ok(Self::default())
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

    pub fn env_pairs(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        self.extend_env(&mut env);
        env
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

fn load_secret_entries(dir: &Path, require_env_var_name: bool) -> Result<Vec<SecretEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("failed to read secrets directory at {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if require_env_var_name && !is_env_var_name(&name) {
            continue;
        }
        let bytes =
            fs::read(&path).with_context(|| format!("failed to read secret {}", path.display()))?;
        let value = String::from_utf8(bytes).ok().map(|v| trim_secret_value(&v));
        entries.push(SecretEntry {
            name: name.clone(),
            rel_path: PathBuf::from(name),
            value,
        });
    }
    Ok(entries)
}

fn trim_secret_value(value: &str) -> String {
    value.trim_end_matches(&['\r', '\n'][..]).to_string()
}

fn is_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::{SECRETS_CONTAINER_DIR, SecretsStore};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn env_pairs_include_value_and_file_reference() {
        let dir = tempdir().expect("tempdir");
        let secrets_dir = dir.path().join(".opal").join("env");
        fs::create_dir_all(&secrets_dir).expect("create secrets dir");
        fs::write(secrets_dir.join("QUAY_PASSWORD"), "dummy-token").expect("write secret");

        let store = SecretsStore::load(dir.path()).expect("load store");
        let pairs = store.env_pairs();
        assert!(pairs.contains(&("QUAY_PASSWORD".to_string(), "dummy-token".to_string())));
        assert!(pairs.iter().any(|(k, v)| {
            k == "QUAY_PASSWORD_FILE" && v == &format!("{SECRETS_CONTAINER_DIR}/QUAY_PASSWORD")
        }));
    }

    #[test]
    fn load_supports_legacy_dotopal_secret_files() {
        let dir = tempdir().expect("tempdir");
        let dotopal_dir = dir.path().join(".opal");
        fs::create_dir_all(&dotopal_dir).expect("create .opal dir");
        fs::write(dotopal_dir.join("QUAY_USERNAME"), "robot-user\n").expect("write secret");
        fs::write(dotopal_dir.join("config.toml"), "ignored=true").expect("write config");

        let store = SecretsStore::load(dir.path()).expect("load store");
        let pairs = store.env_pairs();
        assert!(pairs.contains(&("QUAY_USERNAME".to_string(), "robot-user".to_string())));
        assert!(!pairs.iter().any(|(k, _)| k == "config.toml"));
        assert_eq!(
            store.volume_mount().map(|(host, _)| host),
            Some(dotopal_dir.clone())
        );
    }

    #[test]
    fn scoped_env_dir_precedence_over_legacy_dotopal() {
        let dir = tempdir().expect("tempdir");
        let dotopal_dir = dir.path().join(".opal");
        let secrets_dir = dotopal_dir.join("env");
        fs::create_dir_all(&secrets_dir).expect("create .opal/env dir");
        fs::write(dotopal_dir.join("QUAY_USERNAME"), "legacy-user").expect("write legacy secret");
        fs::write(secrets_dir.join("QUAY_USERNAME"), "scoped-user").expect("write scoped secret");

        let store = SecretsStore::load(dir.path()).expect("load store");
        let pairs = store.env_pairs();
        assert!(pairs.contains(&("QUAY_USERNAME".to_string(), "scoped-user".to_string())));
        assert!(!pairs.contains(&("QUAY_USERNAME".to_string(), "legacy-user".to_string())));
        assert_eq!(
            store.volume_mount().map(|(host, _)| host),
            Some(secrets_dir)
        );
    }
}
