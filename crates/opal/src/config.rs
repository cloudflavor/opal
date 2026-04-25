use crate::{EngineChoice, EngineKind, runtime};
use anyhow::{Context, Result, anyhow};
use dirs::home_dir;
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OpalConfig {
    pub ai: AiSettingsConfig,
    #[serde(alias = "runner_bootstrap")]
    pub bootstrap: RunnerBootstrapConfig,
    pub container: Option<ContainerEngineConfig>,
    pub sandbox: Option<SandboxEngineConfig>,
    pub env: BTreeMap<String, String>,
    pub jobs: Vec<JobOverrideConfig>,
    #[serde(alias = "engine")]
    pub engines: EngineSettings,
    #[serde(rename = "registry")]
    pub registries: Vec<RegistryAuth>,
}

#[derive(Debug, Clone)]
pub struct EngineSettings {
    pub default: Option<EngineChoice>,
    pub container: Option<ContainerEngineConfig>,
    pub preserve_runtime_objects: bool,
    pub map_host_user: bool,
    pub(crate) map_host_user_set: bool,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            default: None,
            container: None,
            preserve_runtime_objects: false,
            map_host_user: true,
            map_host_user_set: false,
        }
    }
}

impl<'de> Deserialize<'de> for EngineSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct RawEngineSettings {
            default: Option<EngineChoice>,
            container: Option<ContainerEngineConfig>,
            preserve_runtime_objects: bool,
            map_host_user: Option<bool>,
        }

        let raw = RawEngineSettings::deserialize(deserializer)?;
        Ok(Self {
            default: raw.default,
            container: raw.container,
            preserve_runtime_objects: raw.preserve_runtime_objects,
            map_host_user: raw.map_host_user.unwrap_or(true),
            map_host_user_set: raw.map_host_user.is_some(),
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RunnerBootstrapConfig {
    pub enabled: Option<bool>,
    pub command: Option<String>,
    #[serde(alias = "dotenv")]
    pub env_file: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub mounts: Vec<RunnerBootstrapMountConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RunnerBootstrapMountConfig {
    pub host: PathBuf,
    pub container: PathBuf,
    pub read_only: bool,
}

impl Default for RunnerBootstrapMountConfig {
    fn default() -> Self {
        Self {
            host: PathBuf::new(),
            container: PathBuf::new(),
            read_only: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiSettingsConfig {
    pub default_provider: Option<AiProviderConfig>,
    pub tail_lines: usize,
    pub save_analysis: bool,
    pub prompts: AiPromptConfig,
    pub claude: ClaudeAiConfig,
    pub codex: CodexAiConfig,
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
            claude: ClaudeAiConfig::default(),
            codex: CodexAiConfig::default(),
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
            claude: ClaudeAiConfig,
            codex: CodexAiConfig,
            ollama: OllamaAiConfig,
        }

        let raw = RawAiSettingsConfig::deserialize(deserializer)?;
        let mut settings = AiSettingsConfig {
            default_provider: raw.default_provider,
            prompts: raw.prompts,
            claude: raw.claude,
            codex: raw.codex,
            ollama: raw.ollama,
            ..AiSettingsConfig::default()
        };
        if raw.tail_lines != 0 {
            settings.tail_lines = raw.tail_lines;
        }
        if let Some(value) = raw.save_analysis {
            settings.save_analysis = value;
            settings.save_analysis_override = Some(value);
        }
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClaudeAiConfig {
    pub command: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CodexAiConfig {
    pub command: String,
    pub model: Option<String>,
}

impl Default for ClaudeAiConfig {
    fn default() -> Self {
        Self {
            command: "claude".to_string(),
            model: None,
        }
    }
}

impl Default for CodexAiConfig {
    fn default() -> Self {
        Self {
            command: "codex".to_string(),
            model: None,
        }
    }
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
pub struct SandboxEngineConfig {
    pub settings: Option<String>,
    pub debug: Option<bool>,
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    pub allow_unix_sockets: Vec<String>,
    pub allow_all_unix_sockets: Option<bool>,
    pub allow_local_binding: Option<bool>,
    pub deny_read: Vec<String>,
    pub allow_read: Vec<String>,
    pub allow_write: Vec<String>,
    pub deny_write: Vec<String>,
    pub ignore_violations: BTreeMap<String, Vec<String>>,
    pub enable_weaker_nested_sandbox: Option<bool>,
    pub mandatory_deny_search_depth: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct JobOverrideConfig {
    pub name: String,
    pub engine: Option<EngineChoice>,
    pub sandbox_settings: Option<String>,
    pub sandbox_debug: Option<bool>,
    pub sandbox_allowed_domains: Vec<String>,
    pub sandbox_denied_domains: Vec<String>,
    pub sandbox_allow_unix_sockets: Vec<String>,
    pub sandbox_allow_all_unix_sockets: Option<bool>,
    pub sandbox_allow_local_binding: Option<bool>,
    pub sandbox_deny_read: Vec<String>,
    pub sandbox_allow_read: Vec<String>,
    pub sandbox_allow_write: Vec<String>,
    pub sandbox_deny_write: Vec<String>,
    pub sandbox_ignore_violations: BTreeMap<String, Vec<String>>,
    pub sandbox_enable_weaker_nested_sandbox: Option<bool>,
    pub sandbox_mandatory_deny_search_depth: Option<u64>,
    pub arch: Option<String>,
    pub privileged: Option<bool>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedJobOverride {
    pub engine: Option<EngineChoice>,
    pub sandbox_settings: Option<String>,
    pub sandbox_debug: Option<bool>,
    pub sandbox_allowed_domains: Vec<String>,
    pub sandbox_denied_domains: Vec<String>,
    pub sandbox_allow_unix_sockets: Vec<String>,
    pub sandbox_allow_all_unix_sockets: Option<bool>,
    pub sandbox_allow_local_binding: Option<bool>,
    pub sandbox_deny_read: Vec<String>,
    pub sandbox_allow_read: Vec<String>,
    pub sandbox_allow_write: Vec<String>,
    pub sandbox_deny_write: Vec<String>,
    pub sandbox_ignore_violations: BTreeMap<String, Vec<String>>,
    pub sandbox_enable_weaker_nested_sandbox: Option<bool>,
    pub sandbox_mandatory_deny_search_depth: Option<u64>,
    pub arch: Option<String>,
    pub privileged: Option<bool>,
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
        let project_config_path = workdir.join(".opal").join("config.toml");
        for path in runtime::config_dirs(workdir) {
            if path.exists() {
                let contents = fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let mut parsed: OpalConfig = toml::from_str(&contents)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                parsed.resolve_relative_paths(&path);
                merged.merge_from_source(parsed, path == project_config_path);
            }
        }
        Ok(merged)
    }

    pub async fn load_async(workdir: &Path) -> Result<Self> {
        let mut merged = OpalConfig::default();
        let project_config_path = workdir.join(".opal").join("config.toml");
        for path in runtime::config_dirs(workdir) {
            let contents = match tokio::fs::read_to_string(&path).await {
                Ok(contents) => contents,
                Err(err) if err.kind() == ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(err).with_context(|| format!("failed to read {}", path.display()));
                }
            };
            let mut parsed: OpalConfig = toml::from_str(&contents)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            parsed.resolve_relative_paths(&path);
            merged.merge_from_source(parsed, path == project_config_path);
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

    pub fn sandbox_settings(&self) -> Option<&SandboxEngineConfig> {
        self.sandbox.as_ref()
    }

    pub fn preserve_runtime_objects(&self) -> bool {
        self.engines.preserve_runtime_objects
    }

    pub fn map_host_user(&self) -> bool {
        self.engines.map_host_user
    }

    pub fn ai_settings(&self) -> &AiSettingsConfig {
        &self.ai
    }

    pub fn configured_env(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    pub fn bootstrap_settings(&self) -> &RunnerBootstrapConfig {
        &self.bootstrap
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
            if let Some(value) = entry.engine {
                resolved.engine = Some(value);
            }
            if let Some(value) = &entry.sandbox_settings {
                resolved.sandbox_settings = Some(value.clone());
            }
            if let Some(value) = entry.sandbox_debug {
                resolved.sandbox_debug = Some(value);
            }
            if !entry.sandbox_allowed_domains.is_empty() {
                resolved.sandbox_allowed_domains = entry.sandbox_allowed_domains.clone();
            }
            if !entry.sandbox_denied_domains.is_empty() {
                resolved.sandbox_denied_domains = entry.sandbox_denied_domains.clone();
            }
            if !entry.sandbox_allow_unix_sockets.is_empty() {
                resolved.sandbox_allow_unix_sockets = entry.sandbox_allow_unix_sockets.clone();
            }
            if let Some(value) = entry.sandbox_allow_all_unix_sockets {
                resolved.sandbox_allow_all_unix_sockets = Some(value);
            }
            if let Some(value) = entry.sandbox_allow_local_binding {
                resolved.sandbox_allow_local_binding = Some(value);
            }
            if !entry.sandbox_deny_read.is_empty() {
                resolved.sandbox_deny_read = entry.sandbox_deny_read.clone();
            }
            if !entry.sandbox_allow_read.is_empty() {
                resolved.sandbox_allow_read = entry.sandbox_allow_read.clone();
            }
            if !entry.sandbox_allow_write.is_empty() {
                resolved.sandbox_allow_write = entry.sandbox_allow_write.clone();
            }
            if !entry.sandbox_deny_write.is_empty() {
                resolved.sandbox_deny_write = entry.sandbox_deny_write.clone();
            }
            if !entry.sandbox_ignore_violations.is_empty() {
                resolved.sandbox_ignore_violations = entry.sandbox_ignore_violations.clone();
            }
            if let Some(value) = entry.sandbox_enable_weaker_nested_sandbox {
                resolved.sandbox_enable_weaker_nested_sandbox = Some(value);
            }
            if let Some(value) = entry.sandbox_mandatory_deny_search_depth {
                resolved.sandbox_mandatory_deny_search_depth = Some(value);
            }
            if let Some(value) = entry.privileged {
                resolved.privileged = Some(value);
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
        self.bootstrap.merge(other.bootstrap);
        if let Some(new_container) = other.container.take() {
            match &mut self.container {
                Some(existing) => existing.merge(new_container),
                slot @ None => *slot = Some(new_container),
            }
        }
        if let Some(new_sandbox) = other.sandbox.take() {
            match &mut self.sandbox {
                Some(existing) => existing.merge(new_sandbox),
                slot @ None => *slot = Some(new_sandbox),
            }
        }
        self.env.extend(other.env);
        self.engines.merge(other.engines);
        self.jobs.extend(other.jobs);
        self.registries.extend(other.registries);
    }

    fn merge_from_source(&mut self, other: OpalConfig, replace_job_overrides: bool) {
        if replace_job_overrides {
            self.jobs.clear();
        }
        self.merge(other);
    }

    fn resolve_relative_paths(&mut self, config_path: &Path) {
        self.ai.prompts.resolve_relative_paths(config_path);
        self.bootstrap.resolve_relative_paths(config_path);
        if let Some(sandbox) = &mut self.sandbox {
            sandbox.resolve_relative_paths(config_path);
        }
        for job in &mut self.jobs {
            job.resolve_relative_paths(config_path);
        }
    }
}

impl RunnerBootstrapConfig {
    pub fn is_active(&self) -> bool {
        self.enabled.unwrap_or_else(|| {
            self.command.is_some()
                || self.env_file.is_some()
                || !self.env.is_empty()
                || !self.mounts.is_empty()
        })
    }

    fn merge(&mut self, other: RunnerBootstrapConfig) {
        if other.enabled.is_some() {
            self.enabled = other.enabled;
        }
        if other.command.is_some() {
            self.command = other.command;
        }
        if other.env_file.is_some() {
            self.env_file = other.env_file;
        }
        self.env.extend(other.env);
        self.mounts.extend(other.mounts);
    }

    fn resolve_relative_paths(&mut self, config_path: &Path) {
        let Some(base_dir) = config_path.parent() else {
            return;
        };
        if let Some(path) = &mut self.env_file
            && !path.is_absolute()
        {
            *path = base_dir.join(&*path);
        }
        for mount in &mut self.mounts {
            if !mount.host.is_absolute() {
                mount.host = base_dir.join(&mount.host);
            }
        }
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
        self.claude.merge(other.claude);
        self.codex.merge(other.codex);
        self.ollama.merge(other.ollama);
    }
}

impl ClaudeAiConfig {
    fn merge(&mut self, other: ClaudeAiConfig) {
        if !other.command.is_empty() {
            self.command = other.command;
        }
        if other.model.is_some() {
            self.model = other.model;
        }
    }
}

impl CodexAiConfig {
    fn merge(&mut self, other: CodexAiConfig) {
        if !other.command.is_empty() {
            self.command = other.command;
        }
        if other.model.is_some() {
            self.model = other.model;
        }
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

    fn resolve_relative_paths(&mut self, config_path: &Path) {
        let Some(base_dir) = config_path.parent() else {
            return;
        };
        if let Some(path) = &mut self.system_file {
            *path = resolve_path_string(base_dir, path);
        }
        if let Some(path) = &mut self.job_analysis_file {
            *path = resolve_path_string(base_dir, path);
        }
    }
}

fn resolve_path_string(base_dir: &Path, value: &str) -> String {
    if let Some(expanded) = expand_home_prefix(value) {
        return expanded.display().to_string();
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path.display().to_string()
    } else {
        base_dir.join(path).display().to_string()
    }
}

fn current_home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(home_dir)
}

fn expand_home_prefix(value: &str) -> Option<PathBuf> {
    let home = current_home_dir()?;
    if value == "~" {
        return Some(home);
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return Some(home.join(rest));
    }
    None
}

fn expand_home_entries(values: &mut Vec<String>) {
    for entry in values {
        if let Some(expanded) = expand_home_prefix(entry) {
            *entry = expanded.display().to_string();
        }
    }
}

fn expand_home_ignore_violations(map: &mut BTreeMap<String, Vec<String>>) {
    for paths in map.values_mut() {
        expand_home_entries(paths);
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
        if other.map_host_user_set {
            self.map_host_user = other.map_host_user;
            self.map_host_user_set = true;
        }
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

impl SandboxEngineConfig {
    fn merge(&mut self, other: SandboxEngineConfig) {
        if let Some(value) = other.settings {
            self.settings = Some(value);
        }
        if let Some(value) = other.debug {
            self.debug = Some(value);
        }
        if !other.allowed_domains.is_empty() {
            self.allowed_domains = other.allowed_domains;
        }
        if !other.denied_domains.is_empty() {
            self.denied_domains = other.denied_domains;
        }
        if !other.allow_unix_sockets.is_empty() {
            self.allow_unix_sockets = other.allow_unix_sockets;
        }
        if let Some(value) = other.allow_all_unix_sockets {
            self.allow_all_unix_sockets = Some(value);
        }
        if let Some(value) = other.allow_local_binding {
            self.allow_local_binding = Some(value);
        }
        if !other.deny_read.is_empty() {
            self.deny_read = other.deny_read;
        }
        if !other.allow_read.is_empty() {
            self.allow_read = other.allow_read;
        }
        if !other.allow_write.is_empty() {
            self.allow_write = other.allow_write;
        }
        if !other.deny_write.is_empty() {
            self.deny_write = other.deny_write;
        }
        if !other.ignore_violations.is_empty() {
            self.ignore_violations = other.ignore_violations;
        }
        if let Some(value) = other.enable_weaker_nested_sandbox {
            self.enable_weaker_nested_sandbox = Some(value);
        }
        if let Some(value) = other.mandatory_deny_search_depth {
            self.mandatory_deny_search_depth = Some(value);
        }
    }

    fn resolve_relative_paths(&mut self, config_path: &Path) {
        if let Some(path) = &self.settings
            && let Some(base_dir) = config_path.parent()
        {
            self.settings = Some(resolve_path_string(base_dir, path));
        }
        expand_home_entries(&mut self.allow_unix_sockets);
        expand_home_entries(&mut self.deny_read);
        expand_home_entries(&mut self.allow_read);
        expand_home_entries(&mut self.allow_write);
        expand_home_entries(&mut self.deny_write);
        expand_home_ignore_violations(&mut self.ignore_violations);
    }
}

impl JobOverrideConfig {
    fn resolve_relative_paths(&mut self, config_path: &Path) {
        if let Some(path) = &self.sandbox_settings
            && let Some(base_dir) = config_path.parent()
        {
            self.sandbox_settings = Some(resolve_path_string(base_dir, path));
        }
        expand_home_entries(&mut self.sandbox_allow_unix_sockets);
        expand_home_entries(&mut self.sandbox_deny_read);
        expand_home_entries(&mut self.sandbox_allow_read);
        expand_home_entries(&mut self.sandbox_allow_write);
        expand_home_entries(&mut self.sandbox_deny_write);
        expand_home_ignore_violations(&mut self.sandbox_ignore_violations);
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
        EngineKind::Sandbox => "sandbox",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContainerEngineConfig, JobOverrideConfig, OpalConfig, RegistryAuth, RunnerBootstrapConfig,
    };
    use crate::EngineKind;
    use std::collections::BTreeMap;
    use std::env;
    use std::path::Path;

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
                    engine: Some(crate::EngineChoice::Docker),
                    sandbox_settings: None,
                    sandbox_debug: None,
                    sandbox_allowed_domains: Vec::new(),
                    sandbox_denied_domains: Vec::new(),
                    sandbox_allow_unix_sockets: Vec::new(),
                    sandbox_allow_all_unix_sockets: None,
                    sandbox_allow_local_binding: None,
                    sandbox_deny_read: Vec::new(),
                    sandbox_allow_read: Vec::new(),
                    sandbox_allow_write: Vec::new(),
                    sandbox_deny_write: Vec::new(),
                    sandbox_ignore_violations: BTreeMap::new(),
                    sandbox_enable_weaker_nested_sandbox: None,
                    sandbox_mandatory_deny_search_depth: None,
                    arch: Some("arm64".into()),
                    privileged: Some(false),
                    cap_add: Vec::new(),
                    cap_drop: Vec::new(),
                },
                JobOverrideConfig {
                    name: "deploy".into(),
                    engine: Some(crate::EngineChoice::Sandbox),
                    sandbox_settings: None,
                    sandbox_debug: None,
                    sandbox_allowed_domains: Vec::new(),
                    sandbox_denied_domains: Vec::new(),
                    sandbox_allow_unix_sockets: Vec::new(),
                    sandbox_allow_all_unix_sockets: None,
                    sandbox_allow_local_binding: None,
                    sandbox_deny_read: Vec::new(),
                    sandbox_allow_read: Vec::new(),
                    sandbox_allow_write: Vec::new(),
                    sandbox_deny_write: Vec::new(),
                    sandbox_ignore_violations: BTreeMap::new(),
                    sandbox_enable_weaker_nested_sandbox: None,
                    sandbox_mandatory_deny_search_depth: None,
                    arch: None,
                    privileged: Some(true),
                    cap_add: vec!["NET_ADMIN".into()],
                    cap_drop: vec!["MKNOD".into()],
                },
            ],
            ..OpalConfig::default()
        };

        let resolved = config.job_override_for("deploy").expect("override present");
        assert_eq!(resolved.engine, Some(crate::EngineChoice::Sandbox));
        assert_eq!(resolved.arch.as_deref(), Some("arm64"));
        assert_eq!(resolved.privileged, Some(true));
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
    fn project_engine_default_overrides_global() {
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
    fn project_jobs_replace_global_job_overrides() {
        let mut base = OpalConfig::default();
        base.merge_from_source(
            toml::from_str(
                r#"
[[jobs]]
name = "e2e-services"
engine = "sandbox"
"#,
            )
            .expect("parse global config"),
            false,
        );
        assert_eq!(
            base.job_override_for("e2e-services")
                .and_then(|override_cfg| override_cfg.engine),
            Some(crate::EngineChoice::Sandbox)
        );

        base.merge_from_source(
            toml::from_str(
                r#"
[[jobs]]
name = "extended-tests"
engine = "sandbox"

[[jobs]]
name = "e2e-tests"
engine = "sandbox"
"#,
            )
            .expect("parse project config"),
            true,
        );

        assert!(base.job_override_for("e2e-services").is_none());
        assert_eq!(
            base.job_override_for("extended-tests")
                .and_then(|override_cfg| override_cfg.engine),
            Some(crate::EngineChoice::Sandbox)
        );
        assert_eq!(
            base.job_override_for("e2e-tests")
                .and_then(|override_cfg| override_cfg.engine),
            Some(crate::EngineChoice::Sandbox)
        );
    }

    #[test]
    fn parses_root_level_env_table() {
        let parsed: OpalConfig = toml::from_str(
            r#"
[env]
RUNNER_BOOTSTRAP = "enabled"
INIT_SCRIPT = "/opal/bootstrap/init.sh"
"#,
        )
        .expect("parse config");

        assert_eq!(
            parsed
                .configured_env()
                .get("RUNNER_BOOTSTRAP")
                .map(String::as_str),
            Some("enabled")
        );
        assert_eq!(
            parsed
                .configured_env()
                .get("INIT_SCRIPT")
                .map(String::as_str),
            Some("/opal/bootstrap/init.sh")
        );
    }

    #[test]
    fn project_env_overrides_global_values() {
        let mut base = OpalConfig {
            env: BTreeMap::from([
                ("RUNNER_BOOTSTRAP".into(), "global".into()),
                ("GLOBAL_ONLY".into(), "1".into()),
            ]),
            ..OpalConfig::default()
        };
        base.merge(OpalConfig {
            env: BTreeMap::from([
                ("RUNNER_BOOTSTRAP".into(), "project".into()),
                ("PROJECT_ONLY".into(), "1".into()),
            ]),
            ..OpalConfig::default()
        });

        assert_eq!(
            base.configured_env()
                .get("RUNNER_BOOTSTRAP")
                .map(String::as_str),
            Some("project")
        );
        assert_eq!(
            base.configured_env().get("GLOBAL_ONLY").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            base.configured_env()
                .get("PROJECT_ONLY")
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn parses_bootstrap_config_with_command_env_and_mounts() {
        let parsed: OpalConfig = toml::from_str(
            r#"
[bootstrap]
command = "bash .opal/bootstrap/setup.sh"
env_file = ".opal/bootstrap/generated.env"

[bootstrap.env]
RUNNER_SCRIPT = "/opal/bootstrap/scripts/init.sh"

[[bootstrap.mounts]]
host = ".opal/bootstrap/scripts"
container = "/opal/bootstrap/scripts"
read_only = true
"#,
        )
        .expect("parse config");

        let bootstrap = parsed.bootstrap_settings();
        assert_eq!(
            bootstrap.command.as_deref(),
            Some("bash .opal/bootstrap/setup.sh")
        );
        assert_eq!(
            bootstrap.env.get("RUNNER_SCRIPT").map(String::as_str),
            Some("/opal/bootstrap/scripts/init.sh")
        );
        assert_eq!(bootstrap.mounts.len(), 1);
        assert_eq!(
            bootstrap.mounts[0].container.as_os_str().to_string_lossy(),
            "/opal/bootstrap/scripts"
        );
    }

    #[test]
    fn bootstrap_relative_paths_resolve_from_config_dir() {
        let mut parsed: OpalConfig = toml::from_str(
            r#"
[bootstrap]
env_file = "runtime/bootstrap.env"

[[bootstrap.mounts]]
host = "runtime/scripts"
container = "/opal/bootstrap/scripts"
"#,
        )
        .expect("parse config");

        parsed.resolve_relative_paths(Path::new("/tmp/project/.opal/config.toml"));

        assert_eq!(
            parsed
                .bootstrap_settings()
                .env_file
                .as_ref()
                .map(|p| p.display().to_string())
                .as_deref(),
            Some("/tmp/project/.opal/runtime/bootstrap.env")
        );
        assert_eq!(
            parsed.bootstrap_settings().mounts[0]
                .host
                .display()
                .to_string(),
            "/tmp/project/.opal/runtime/scripts"
        );
    }

    #[test]
    fn bootstrap_can_be_explicitly_disabled() {
        let parsed: OpalConfig = toml::from_str(
            r#"
[bootstrap]
enabled = false
command = "echo should-not-run"
"#,
        )
        .expect("parse config");

        assert!(!parsed.bootstrap_settings().is_active());
    }

    #[test]
    fn bootstrap_defaults_to_active_when_configured() {
        let bootstrap = RunnerBootstrapConfig {
            command: Some("echo hi".into()),
            ..RunnerBootstrapConfig::default()
        };
        assert!(bootstrap.is_active());
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
    fn parses_map_host_user_from_engine_table() {
        let parsed: OpalConfig = toml::from_str(
            r#"
[engine]
map_host_user = true
"#,
        )
        .expect("parse config");

        assert!(parsed.map_host_user());
    }

    #[test]
    fn map_host_user_defaults_to_true() {
        assert!(OpalConfig::default().map_host_user());
    }

    #[test]
    fn parses_map_host_user_false_from_engine_table() {
        let parsed: OpalConfig = toml::from_str(
            r#"
[engine]
map_host_user = false
"#,
        )
        .expect("parse config");

        assert!(!parsed.map_host_user());
    }

    #[test]
    fn project_map_host_user_setting_overrides_global() {
        let mut base = OpalConfig::default();
        base.merge(
            toml::from_str(
                r#"
[engine]
map_host_user = true
"#,
            )
            .expect("parse global config"),
        );
        base.merge(
            toml::from_str(
                r#"
[engine]
map_host_user = false
"#,
            )
            .expect("parse project config"),
        );

        assert!(!base.map_host_user());
    }

    #[test]
    fn ai_settings_default_to_ollama_friendly_values() {
        let settings = OpalConfig::default();
        assert_eq!(settings.ai.tail_lines, 200);
        assert!(settings.ai.save_analysis);
        assert_eq!(settings.ai.claude.command, "claude");
        assert!(settings.ai.claude.model.is_none());
        assert_eq!(settings.ai.codex.command, "codex");
        assert!(settings.ai.codex.model.is_none());
        assert_eq!(settings.ai.ollama.host, "http://127.0.0.1:11434");
        assert!(settings.ai.ollama.model.is_empty());
    }

    #[test]
    fn ai_prompt_paths_resolve_relative_to_config_file_directory() {
        let mut parsed: OpalConfig = toml::from_str(
            r#"
[ai.prompts]
system_file = "prompts/ai/system.md"
job_analysis_file = "prompts/ai/job-analysis.md"
"#,
        )
        .expect("parse config");

        parsed.resolve_relative_paths(Path::new("/tmp/project/.opal/config.toml"));

        assert_eq!(
            parsed.ai.prompts.system_file.as_deref(),
            Some("/tmp/project/.opal/prompts/ai/system.md")
        );
        assert_eq!(
            parsed.ai.prompts.job_analysis_file.as_deref(),
            Some("/tmp/project/.opal/prompts/ai/job-analysis.md")
        );
    }

    #[test]
    fn sandbox_paths_expand_home_prefixes() {
        let prior_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", "/tmp/opal-home");
        }

        let mut parsed: OpalConfig = toml::from_str(
            r#"
[sandbox]
allow_all_unix_sockets = true
allow_unix_sockets = ["~/.docker/run/docker.sock"]
deny_read = ["~/.ssh"]
allow_read = ["~/.ssh/known_hosts"]
allow_write = ["~/.local/share/opal"]
deny_write = ["~/.env"]
ignore_violations = { "*" = ["~/.cache/opal"] }

[[jobs]]
name = "extended-tests"
sandbox_allow_all_unix_sockets = false
sandbox_allow_unix_sockets = ["~/.containerd-rootless/grpc.sock"]
sandbox_deny_read = ["~/.gitconfig"]
sandbox_allow_read = ["~/.ssh/known_hosts"]
sandbox_allow_write = ["~/.local/share/opal"]
sandbox_deny_write = ["~/.env"]
sandbox_ignore_violations = { "*" = ["~/.cache/opal"] }
"#,
        )
        .expect("parse config");

        parsed.resolve_relative_paths(Path::new("/tmp/project/.opal/config.toml"));

        let sandbox = parsed.sandbox_settings().expect("sandbox settings");
        assert_eq!(
            sandbox.allow_unix_sockets,
            vec!["/tmp/opal-home/.docker/run/docker.sock"]
        );
        assert_eq!(sandbox.allow_all_unix_sockets, Some(true));
        assert_eq!(sandbox.deny_read, vec!["/tmp/opal-home/.ssh"]);
        assert_eq!(sandbox.allow_read, vec!["/tmp/opal-home/.ssh/known_hosts"]);
        assert_eq!(
            sandbox.allow_write,
            vec!["/tmp/opal-home/.local/share/opal"]
        );
        assert_eq!(sandbox.deny_write, vec!["/tmp/opal-home/.env"]);
        assert_eq!(
            sandbox
                .ignore_violations
                .get("*")
                .cloned()
                .unwrap_or_default(),
            vec!["/tmp/opal-home/.cache/opal"]
        );

        let override_cfg = parsed
            .job_override_for("extended-tests")
            .expect("job override present");
        assert_eq!(
            override_cfg.sandbox_allow_unix_sockets,
            vec!["/tmp/opal-home/.containerd-rootless/grpc.sock"]
        );
        assert_eq!(override_cfg.sandbox_allow_all_unix_sockets, Some(false));
        assert_eq!(
            override_cfg.sandbox_deny_read,
            vec!["/tmp/opal-home/.gitconfig"]
        );
        assert_eq!(
            override_cfg.sandbox_allow_read,
            vec!["/tmp/opal-home/.ssh/known_hosts"]
        );
        assert_eq!(
            override_cfg.sandbox_allow_write,
            vec!["/tmp/opal-home/.local/share/opal"]
        );
        assert_eq!(override_cfg.sandbox_deny_write, vec!["/tmp/opal-home/.env"]);
        assert_eq!(
            override_cfg
                .sandbox_ignore_violations
                .get("*")
                .cloned()
                .unwrap_or_default(),
            vec!["/tmp/opal-home/.cache/opal"]
        );

        match prior_home {
            Some(value) => unsafe {
                env::set_var("HOME", value);
            },
            None => unsafe {
                env::remove_var("HOME");
            },
        }
    }

    #[test]
    fn registry_entry_with_password_env_is_resolved() {
        let mut base = OpalConfig::default();
        base.registries.push(RegistryAuth {
            server: "registry.example.com".into(),
            username: "ci-token".into(),
            password: None,
            password_env: Some("OPAL_TEST_REGISTRY_PASSWORD".into()),
            engines: Vec::new(),
            scheme: None,
        });

        unsafe {
            env::set_var("OPAL_TEST_REGISTRY_PASSWORD", "sekret");
        }

        let resolved = base
            .registry_auth_for(EngineKind::Docker)
            .expect("password_env should be resolved");

        unsafe {
            env::remove_var("OPAL_TEST_REGISTRY_PASSWORD");
        }

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].server, "registry.example.com");
        assert_eq!(resolved[0].username, "ci-token");
        assert_eq!(resolved[0].password, "sekret");
    }

    #[test]
    fn registry_entry_missing_password_env_returns_error() {
        let mut base = OpalConfig::default();
        base.registries.push(RegistryAuth {
            server: "registry.example.com".into(),
            username: "ci-token".into(),
            password: None,
            password_env: Some("OPAL_TEST_REGISTRY_PASSWORD_MISSING".into()),
            engines: Vec::new(),
            scheme: None,
        });

        unsafe {
            env::remove_var("OPAL_TEST_REGISTRY_PASSWORD_MISSING");
        }
        let err = base
            .registry_auth_for(EngineKind::Docker)
            .expect_err("missing env var should fail");

        assert!(
            err.to_string()
                .contains("missing env var OPAL_TEST_REGISTRY_PASSWORD_MISSING")
        );
    }
}
