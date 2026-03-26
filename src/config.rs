use crate::{EngineKind, runtime};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OpalConfig {
    pub container: Option<ContainerEngineConfig>,
    pub engines: EngineSettings,
    #[serde(rename = "registry")]
    pub registries: Vec<RegistryAuth>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EngineSettings {
    pub container: Option<ContainerEngineConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ContainerEngineConfig {
    pub arch: Option<String>,
    pub cpus: Option<String>,
    pub memory: Option<String>,
    pub dns: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryAuth {
    pub server: String,
    pub username: String,
    pub password: Option<String>,
    pub password_env: Option<String>,
    #[serde(default)]
    pub engines: Vec<String>,
    pub scheme: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedRegistryAuth {
    pub server: String,
    pub username: String,
    pub password: String,
    pub scheme: Option<String>,
}

impl OpalConfig {
    pub fn load(workdir: &Path) -> Result<Self> {
        let mut merged = OpalConfig::default();
        for path in runtime::config_dirs(workdir) {
            if path.exists() {
                let contents = fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let parsed: OpalConfig = toml::from_str(&contents)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                merged.merge(parsed);
            }
        }
        Ok(merged)
    }

    pub fn container_settings(&self) -> Option<&ContainerEngineConfig> {
        if let Some(cfg) = self.container.as_ref() {
            return Some(cfg);
        }
        self.engines.container.as_ref()
    }

    pub fn registry_auth_for(&self, engine: EngineKind) -> Result<Vec<ResolvedRegistryAuth>> {
        let mut seen = HashSet::new();
        let mut results = Vec::new();
        for auth in &self.registries {
            if !auth.applies_to(engine) {
                continue;
            }
            let resolved = auth.resolve()?;
            if seen.insert((resolved.server.clone(), resolved.username.clone())) {
                results.push(resolved);
            }
        }
        Ok(results)
    }

    fn merge(&mut self, mut other: OpalConfig) {
        if let Some(new_container) = other.container.take() {
            match &mut self.container {
                Some(existing) => existing.merge(new_container),
                slot @ None => *slot = Some(new_container),
            }
        }
        self.engines.merge(other.engines);
        self.registries.extend(other.registries);
    }
}

impl EngineSettings {
    fn merge(&mut self, other: EngineSettings) {
        if let Some(new_container) = other.container {
            match &mut self.container {
                Some(existing) => existing.merge(new_container),
                slot @ None => *slot = Some(new_container),
            }
        }
    }
}

impl ContainerEngineConfig {
    fn merge(&mut self, other: ContainerEngineConfig) {
        let ContainerEngineConfig {
            arch,
            cpus,
            memory,
            dns,
        } = other;
        if let Some(value) = arch {
            self.arch = Some(value);
        }
        if let Some(value) = cpus {
            self.cpus = Some(value);
        }
        if let Some(value) = memory {
            self.memory = Some(value);
        }
        if let Some(value) = dns {
            self.dns = Some(value);
        }
    }
}

impl RegistryAuth {
    fn applies_to(&self, engine: EngineKind) -> bool {
        if self.engines.is_empty() {
            return true;
        }
        let target = engine_name(engine);
        self.engines
            .iter()
            .any(|value| value.eq_ignore_ascii_case(target))
    }

    fn resolve(&self) -> Result<ResolvedRegistryAuth> {
        let password = if let Some(env_name) = &self.password_env {
            env::var(env_name).with_context(|| {
                format!(
                    "registry auth for '{}' missing env var {}",
                    self.server, env_name
                )
            })?
        } else if let Some(pass) = &self.password {
            pass.clone()
        } else {
            return Err(anyhow!(
                "registry auth for '{}' must specify password or password_env",
                self.server
            ));
        };
        Ok(ResolvedRegistryAuth {
            server: self.server.clone(),
            username: self.username.clone(),
            password,
            scheme: self.scheme.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ContainerEngineConfig, OpalConfig};

    #[test]
    fn container_config_merge_overrides_arch() {
        let mut base = OpalConfig {
            container: Some(ContainerEngineConfig {
                arch: Some("x86_64".into()),
                cpus: None,
                memory: None,
                dns: None,
            }),
            ..OpalConfig::default()
        };

        base.merge(OpalConfig {
            container: Some(ContainerEngineConfig {
                arch: Some("arm64".into()),
                cpus: None,
                memory: None,
                dns: None,
            }),
            ..OpalConfig::default()
        });

        assert_eq!(
            base.container_settings()
                .and_then(|cfg| cfg.arch.as_deref()),
            Some("arm64")
        );
    }
}

fn engine_name(engine: EngineKind) -> &'static str {
    match engine {
        EngineKind::ContainerCli => "container",
        EngineKind::Docker => "docker",
        EngineKind::Podman => "podman",
        EngineKind::Nerdctl => "nerdctl",
        EngineKind::Orbstack => "orbstack",
    }
}
