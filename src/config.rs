use crate::{EngineChoice, EngineKind, runtime};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OpalConfig {
    pub ai: AiSettingsConfig,
    pub container: Option<ContainerEngineConfig>,
    pub jobs: Vec<JobOverrideConfig>,
    #[serde(alias = "engine")]
    pub engines: EngineSettings,
    #[serde(rename = "registry")]
    pub registries: Vec<RegistryAuth>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EngineSettings {
    pub default: Option<EngineChoice>,
    pub container: Option<ContainerEngineConfig>,
    pub preserve_runtime_objects: bool,
}

#[derive(Debug, Clone)]
pub struct AiSettingsConfig {
    pub default_provider: Option<AiProviderConfig>,
    pub tail_lines: usize,
    pub save_analysis: bool,
    pub prompts: AiPromptConfig,
    pub ollama: OllamaAiConfig,
    save_analysis_override: Option<bool>,
}

impl Default for AiSettingsConfig {
    fn default() -> Self {
        Self {
            default_provider: None,
            tail_lines: 200,
            save_analysis: true,
            prompts: AiPromptConfig::default(),
            ollama: OllamaAiConfig::default(),
            save_analysis_override: None,
        }
    }
}

impl<'de> Deserialize<'de> for AiSettingsConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct RawAiSettingsConfig {
            default_provider: Option<AiProviderConfig>,
            tail_lines: usize,
            save_analysis: Option<bool>,
            prompts: AiPromptConfig,
            ollama: OllamaAiConfig,
        }

        let raw = RawAiSettingsConfig::deserialize(deserializer)?;
        let mut settings = AiSettingsConfig::default();
        settings.default_provider = raw.default_provider;
        if raw.tail_lines != 0 {
            settings.tail_lines = raw.tail_lines;
        }
        if let Some(value) = raw.save_analysis {
            settings.save_analysis = value;
            settings.save_analysis_override = Some(value);
        }
        settings.prompts = raw.prompts;
        settings.ollama = raw.ollama;
        Ok(settings)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AiPromptConfig {
    pub system_file: Option<String>,
    pub job_analysis_file: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AiProviderConfig {
    Ollama,
    Claude,
    Codex,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OllamaAiConfig {
    pub host: String,
    pub model: String,
    pub system: Option<String>,
}

impl Default for OllamaAiConfig {
    fn default() -> Self {
        Self {
            host: "http://127.0.0.1:11434".to_string(),
            model: String::new(),
            system: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ContainerEngineConfig {
    pub arch: Option<String>,
    pub cpus: Option<String>,
    pub memory: Option<String>,
    pub dns: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct JobOverrideConfig {
    pub name: String,
    pub arch: Option<String>,
    pub privileged: Option<bool>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedJobOverride {
    pub arch: Option<String>,
    pub privileged: bool,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
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

    pub fn default_engine(&self) -> Option<EngineChoice> {
        self.engines.default
    }

    pub fn preserve_runtime_objects(&self) -> bool {
        self.engines.preserve_runtime_objects
    }

    pub fn ai_settings(&self) -> &AiSettingsConfig {
        &self.ai
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

    pub fn job_override_for(&self, job_name: &str) -> Option<ResolvedJobOverride> {
        let mut resolved = ResolvedJobOverride::default();
        let mut matched = false;
        for entry in &self.jobs {
            if entry.name != job_name {
                continue;
            }
            matched = true;
            if let Some(value) = &entry.arch {
                resolved.arch = Some(value.clone());
            }
            if let Some(value) = entry.privileged {
                resolved.privileged = value;
            }
            if !entry.cap_add.is_empty() {
                resolved.cap_add = entry.cap_add.clone();
            }
            if !entry.cap_drop.is_empty() {
                resolved.cap_drop = entry.cap_drop.clone();
            }
        }
        matched.then_some(resolved)
    }

    fn merge(&mut self, mut other: OpalConfig) {
        self.ai.merge(other.ai);
        if let Some(new_container) = other.container.take() {
            match &mut self.container {
                Some(existing) => existing.merge(new_container),
                slot @ None => *slot = Some(new_container),
            }
        }
        self.engines.merge(other.engines);
        self.jobs.extend(other.jobs);
        self.registries.extend(other.registries);
    }
}

impl AiSettingsConfig {
    fn merge(&mut self, other: AiSettingsConfig) {
        if let Some(provider) = other.default_provider {
            self.default_provider = Some(provider);
        }
        if other.tail_lines != 0 {
            self.tail_lines = other.tail_lines;
        }
        if let Some(value) = other.save_analysis_override {
            self.save_analysis = value;
            self.save_analysis_override = Some(value);
        }
        self.prompts.merge(other.prompts);
        self.ollama.merge(other.ollama);
    }
}

impl AiPromptConfig {
    fn merge(&mut self, other: AiPromptConfig) {
        if other.system_file.is_some() {
            self.system_file = other.system_file;
        }
        if other.job_analysis_file.is_some() {
            self.job_analysis_file = other.job_analysis_file;
        }
    }
}

impl OllamaAiConfig {
    fn merge(&mut self, other: OllamaAiConfig) {
        if !other.host.is_empty() {
            self.host = other.host;
        }
        if !other.model.is_empty() {
            self.model = other.model;
        }
        if other.system.is_some() {
            self.system = other.system;
        }
    }
}

impl EngineSettings {
    fn merge(&mut self, other: EngineSettings) {
        if let Some(default) = other.default {
            self.default = Some(default);
        }
        self.preserve_runtime_objects =
            self.preserve_runtime_objects || other.preserve_runtime_objects;
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

fn engine_name(engine: EngineKind) -> &'static str {
    match engine {
        EngineKind::ContainerCli => "container",
        EngineKind::Docker => "docker",
        EngineKind::Podman => "podman",
        EngineKind::Nerdctl => "nerdctl",
        EngineKind::Orbstack => "orbstack",
    }
}

#[cfg(test)]
mod tests {
    use super::{ContainerEngineConfig, JobOverrideConfig, OpalConfig};

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

    #[test]
    fn job_override_for_merges_matching_entries() {
        let config = OpalConfig {
            jobs: vec![
                JobOverrideConfig {
                    name: "deploy".into(),
                    arch: Some("arm64".into()),
                    privileged: Some(false),
                    cap_add: Vec::new(),
                    cap_drop: Vec::new(),
                },
                JobOverrideConfig {
                    name: "deploy".into(),
                    arch: None,
                    privileged: Some(true),
                    cap_add: vec!["NET_ADMIN".into()],
                    cap_drop: vec!["MKNOD".into()],
                },
            ],
            ..OpalConfig::default()
        };

        let resolved = config.job_override_for("deploy").expect("override present");
        assert_eq!(resolved.arch.as_deref(), Some("arm64"));
        assert!(resolved.privileged);
        assert_eq!(resolved.cap_add, vec!["NET_ADMIN"]);
        assert_eq!(resolved.cap_drop, vec!["MKNOD"]);
    }

    #[test]
    fn parses_default_engine_from_engine_table() {
        let parsed: OpalConfig = toml::from_str(
            r#"
[engine]
default = "docker"
"#,
        )
        .expect("parse config");

        assert_eq!(parsed.default_engine(), Some(crate::EngineChoice::Docker));
    }

    #[test]
    fn project_level_engine_default_overrides_global() {
        let mut base = OpalConfig::default();
        base.merge(
            toml::from_str(
                r#"
[engine]
default = "docker"
"#,
            )
            .expect("parse global config"),
        );
        base.merge(
            toml::from_str(
                r#"
[engine]
default = "container"
"#,
            )
            .expect("parse project config"),
        );

        assert_eq!(base.default_engine(), Some(crate::EngineChoice::Container));
    }

    #[test]
    fn parses_preserve_runtime_objects_from_engine_table() {
        let parsed: OpalConfig = toml::from_str(
            r#"
[engine]
preserve_runtime_objects = true
"#,
        )
        .expect("parse config");

        assert!(parsed.preserve_runtime_objects());
    }

    #[test]
    fn ai_settings_default_to_ollama_friendly_values() {
        let settings = OpalConfig::default();
        assert_eq!(settings.ai.tail_lines, 200);
        assert!(settings.ai.save_analysis);
        assert_eq!(settings.ai.ollama.host, "http://127.0.0.1:11434");
        assert!(settings.ai.ollama.model.is_empty());
    }
}
