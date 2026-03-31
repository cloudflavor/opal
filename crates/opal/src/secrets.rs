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
    rel_path: Option<PathBuf>,
    value: Option<String>,
}

impl SecretsStore {
    pub fn load(workdir: &Path) -> Result<Self> {
        let scoped_path = workdir.join(SECRETS_RELATIVE_DIR);
        if scoped_path.exists() {
            if scoped_path.is_dir() {
                return Ok(Self {
                    root: Some(scoped_path.clone()),
                    entries: load_secret_entries(&scoped_path)?,
                });
            }
            if scoped_path.is_file() {
                let entries = load_dotenv_file_entries(&scoped_path)?;
                if !entries.is_empty() {
                    return Ok(Self {
                        root: None,
                        entries,
                    });
                }
            }
        }

        let legacy_dir = workdir.join(LEGACY_SECRETS_RELATIVE_DIR);
        if legacy_dir.exists() && legacy_dir.is_dir() {
            let entries = load_secret_entries(&legacy_dir)?;
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
                upsert_env(env, &entry.name, value);
            }
            if let Some(rel_path) = &entry.rel_path {
                let file_env = format!("{}_FILE", entry.name);
                let file_path = Path::new(SECRETS_CONTAINER_DIR).join(rel_path);
                upsert_env(env, &file_env, &file_path.display().to_string());
            }
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

fn load_secret_entries(dir: &Path) -> Result<Vec<SecretEntry>> {
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
        if !is_env_var_name(&name) {
            continue;
        }
        let bytes =
            fs::read(&path).with_context(|| format!("failed to read secret {}", path.display()))?;
        let value = String::from_utf8(bytes).ok().map(|v| trim_secret_value(&v));
        entries.push(SecretEntry {
            name: name.clone(),
            rel_path: Some(PathBuf::from(name)),
            value,
        });
    }
    Ok(entries)
}

fn load_dotenv_file_entries(path: &Path) -> Result<Vec<SecretEntry>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read dotenv secrets file at {}", path.display()))?;
    let mut entries = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        if !is_env_var_name(key) {
            continue;
        }
        let value = parse_dotenv_value(raw_value.trim());
        entries.push(SecretEntry {
            name: key.to_string(),
            rel_path: None,
            value: Some(value),
        });
    }
    Ok(entries)
}

pub fn load_dotenv_env_pairs(path: &Path) -> Result<Vec<(String, String)>> {
    Ok(load_dotenv_file_entries(path)?
        .into_iter()
        .filter_map(|entry| entry.value.map(|value| (entry.name, value)))
        .collect())
}

fn trim_secret_value(value: &str) -> String {
    value.trim_end_matches(&['\r', '\n'][..]).to_string()
}

fn parse_dotenv_value(value: &str) -> String {
    let unquoted = if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    };
    trim_secret_value(unquoted)
}

fn upsert_env(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some((_, existing_value)) = env.iter_mut().find(|(existing_key, _)| existing_key == key)
    {
        *existing_value = value.to_string();
    } else {
        env.push((key.to_string(), value.to_string()));
    }
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
    use anyhow::Result;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn env_pairs_include_value_and_file_reference() -> Result<()> {
        let dir = tempdir()?;
        let secrets_dir = dir.path().join(".opal").join("env");
        fs::create_dir_all(&secrets_dir)?;
        fs::write(secrets_dir.join("QUAY_PASSWORD"), "dummy-token")?;

        let store = SecretsStore::load(dir.path())?;
        let pairs = store.env_pairs();
        assert!(pairs.contains(&("QUAY_PASSWORD".to_string(), "dummy-token".to_string())));
        assert!(pairs.iter().any(|(k, v)| {
            k == "QUAY_PASSWORD_FILE" && v == &format!("{SECRETS_CONTAINER_DIR}/QUAY_PASSWORD")
        }));
        Ok(())
    }

    #[test]
    fn load_supports_legacy_dotopal_secret_files() -> Result<()> {
        let dir = tempdir()?;
        let dotopal_dir = dir.path().join(".opal");
        fs::create_dir_all(&dotopal_dir)?;
        fs::write(dotopal_dir.join("QUAY_USERNAME"), "robot-user\n")?;
        fs::write(dotopal_dir.join("config.toml"), "ignored=true")?;

        let store = SecretsStore::load(dir.path())?;
        let pairs = store.env_pairs();
        assert!(pairs.contains(&("QUAY_USERNAME".to_string(), "robot-user".to_string())));
        assert!(!pairs.iter().any(|(k, _)| k == "config.toml"));
        assert_eq!(
            store.volume_mount().map(|(host, _)| host),
            Some(dotopal_dir.clone())
        );
        Ok(())
    }

    #[test]
    fn scoped_env_dir_precedence_over_legacy_dotopal() -> Result<()> {
        let dir = tempdir()?;
        let dotopal_dir = dir.path().join(".opal");
        let secrets_dir = dotopal_dir.join("env");
        fs::create_dir_all(&secrets_dir)?;
        fs::write(dotopal_dir.join("QUAY_USERNAME"), "legacy-user")?;
        fs::write(secrets_dir.join("QUAY_USERNAME"), "scoped-user")?;

        let store = SecretsStore::load(dir.path())?;
        let pairs = store.env_pairs();
        assert!(pairs.contains(&("QUAY_USERNAME".to_string(), "scoped-user".to_string())));
        assert!(!pairs.contains(&("QUAY_USERNAME".to_string(), "legacy-user".to_string())));
        assert_eq!(
            store.volume_mount().map(|(host, _)| host),
            Some(secrets_dir)
        );
        Ok(())
    }

    #[test]
    fn scoped_env_dir_ignores_non_env_file_names() -> Result<()> {
        let dir = tempdir()?;
        let secrets_dir = dir.path().join(".opal").join("env");
        fs::create_dir_all(&secrets_dir)?;
        fs::write(secrets_dir.join("QUAY_USERNAME"), "scoped-user")?;
        fs::write(secrets_dir.join("config.toml"), "ignored=true")?;

        let store = SecretsStore::load(dir.path())?;
        let pairs = store.env_pairs();
        assert!(pairs.contains(&("QUAY_USERNAME".to_string(), "scoped-user".to_string())));
        assert!(!pairs.iter().any(|(key, _)| key == "config.toml"));
        Ok(())
    }

    #[test]
    fn load_supports_dotenv_file_under_dotopal_env() -> Result<()> {
        let dir = tempdir()?;
        let dotopal_dir = dir.path().join(".opal");
        fs::create_dir_all(&dotopal_dir)?;
        fs::write(
            dotopal_dir.join("env"),
            "export QUAY_USERNAME=robot-user\nQUAY_PASSWORD=\"dummy-token\"\n",
        )?;

        let store = SecretsStore::load(dir.path())?;
        let pairs = store.env_pairs();
        assert!(pairs.contains(&("QUAY_USERNAME".to_string(), "robot-user".to_string())));
        assert!(pairs.contains(&("QUAY_PASSWORD".to_string(), "dummy-token".to_string())));
        assert!(!pairs.iter().any(|(key, _)| key.ends_with("_FILE")));
        assert!(store.volume_mount().is_none());
        Ok(())
    }

    #[test]
    fn secret_values_override_existing_env_entries() -> Result<()> {
        let dir = tempdir()?;
        let secrets_dir = dir.path().join(".opal").join("env");
        fs::create_dir_all(&secrets_dir)?;
        fs::write(secrets_dir.join("QUAY_USERNAME"), "secret-user")?;

        let store = SecretsStore::load(dir.path())?;
        let mut env = vec![("QUAY_USERNAME".to_string(), "".to_string())];
        store.extend_env(&mut env);

        assert!(env.contains(&("QUAY_USERNAME".to_string(), "secret-user".to_string())));
        let username_count = env.iter().filter(|(k, _)| k == "QUAY_USERNAME").count();
        assert_eq!(username_count, 1);
        Ok(())
    }
}
