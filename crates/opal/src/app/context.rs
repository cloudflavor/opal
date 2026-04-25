use crate::config::OpalConfig;
use crate::git;
use crate::pipeline::RuleContext;
use crate::secrets::SecretsStore;
use crate::{EngineChoice, EngineKind, GitLabRemoteConfig};
use anyhow::Result;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn resolve_pipeline_path(workdir: &Path, pipeline: Option<PathBuf>) -> PathBuf {
    pipeline.unwrap_or_else(|| workdir.join(".gitlab-ci.yml"))
}

pub(crate) fn resolve_gitlab_remote(
    base_url: Option<String>,
    token: Option<String>,
) -> Option<GitLabRemoteConfig> {
    token.map(|token| GitLabRemoteConfig {
        base_url: base_url
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| "https://gitlab.com".to_string()),
        token,
    })
}

pub(crate) fn rule_context_for_workdir(workdir: &Path) -> RuleContext {
    let mut ctx_env: HashMap<String, String> = env::vars().collect();
    let run_manual = env::var("OPAL_RUN_MANUAL").is_ok_and(|v| v == "1");
    if let Ok(secrets) = SecretsStore::load(workdir) {
        ctx_env.extend(secrets.env_pairs());
    }
    RuleContext::from_env(workdir, ctx_env, run_manual)
}

pub(crate) fn history_scope_root(workdir: &Path) -> String {
    let root = git::repository_root(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    fs::canonicalize(&root)
        .unwrap_or(root)
        .display()
        .to_string()
}

pub(crate) fn resolve_engine_choice(choice: EngineChoice, settings: &OpalConfig) -> EngineChoice {
    if choice != EngineChoice::Auto {
        return choice;
    }
    settings.default_engine().unwrap_or(EngineChoice::Auto)
}

#[cfg(target_os = "macos")]
pub(crate) fn validate_engine_choice(choice: EngineChoice) -> Result<()> {
    if matches!(choice, EngineChoice::Nerdctl) {
        anyhow::bail!(
            "'nerdctl' is treated as a Linux-specific engine; on macOS use 'docker', 'orbstack', or 'container'"
        );
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn validate_engine_choice(_: EngineChoice) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn resolve_engine(choice: EngineChoice) -> EngineKind {
    match choice {
        EngineChoice::Auto => EngineKind::ContainerCli,
        EngineChoice::Container => EngineKind::ContainerCli,
        EngineChoice::Docker => EngineKind::Docker,
        EngineChoice::Orbstack => EngineKind::Orbstack,
        EngineChoice::Podman => EngineKind::Podman,
        EngineChoice::Nerdctl => EngineKind::Nerdctl,
        EngineChoice::Sandbox => EngineKind::Sandbox,
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn resolve_engine(choice: EngineChoice) -> EngineKind {
    match choice {
        EngineChoice::Auto | EngineChoice::Podman => EngineKind::Podman,
        EngineChoice::Docker => EngineKind::Docker,
        EngineChoice::Nerdctl => EngineKind::Nerdctl,
        EngineChoice::Orbstack => EngineKind::Docker,
        EngineChoice::Sandbox => EngineKind::Sandbox,
        EngineChoice::Container => {
            eprintln!("'container' engine is unavailable on Linux; falling back to docker");
            EngineKind::Docker
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn resolve_engine(_: EngineChoice) -> EngineKind {
    EngineKind::Docker
}

#[cfg(test)]
mod tests {
    use super::{history_scope_root, resolve_engine_choice, rule_context_for_workdir};
    use crate::EngineChoice;
    use crate::config::{EngineSettings, OpalConfig};
    use crate::git::test_support::init_repo_with_commit_and_tag;
    use anyhow::Result;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn rule_context_includes_opal_env_values() -> Result<()> {
        let dir = tempdir()?;
        let secrets_dir = dir.path().join(".opal").join("env");
        fs::create_dir_all(&secrets_dir)?;
        fs::write(secrets_dir.join("QUAY_USERNAME"), "robot-user")?;

        let ctx = rule_context_for_workdir(dir.path());
        assert_eq!(ctx.env_value("QUAY_USERNAME"), Some("robot-user"));
        Ok(())
    }

    #[test]
    fn explicit_engine_choice_wins_over_config_default() {
        let settings = OpalConfig {
            engines: EngineSettings {
                default: Some(EngineChoice::Docker),
                ..EngineSettings::default()
            },
            ..OpalConfig::default()
        };

        assert_eq!(
            resolve_engine_choice(EngineChoice::Podman, &settings),
            EngineChoice::Podman
        );
    }

    #[test]
    fn config_default_engine_is_used_when_cli_is_auto() {
        let settings = OpalConfig {
            engines: EngineSettings {
                default: Some(EngineChoice::Docker),
                ..EngineSettings::default()
            },
            ..OpalConfig::default()
        };

        assert_eq!(
            resolve_engine_choice(EngineChoice::Auto, &settings),
            EngineChoice::Docker
        );
    }

    #[test]
    fn history_scope_uses_repository_root_when_available() -> Result<()> {
        let dir = init_repo_with_commit_and_tag("v0.1.0")?;
        let nested = dir.path().join("nested").join("child");
        fs::create_dir_all(&nested)?;

        assert_eq!(
            history_scope_root(&nested),
            dir.path().canonicalize()?.display().to_string()
        );
        Ok(())
    }
}
