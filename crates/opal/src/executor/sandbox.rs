use super::core::{ExecutionOutcome, ExecutorCore};
use crate::config::{ResolvedJobOverride, SandboxEngineConfig};
use crate::engine::EngineCommandContext;
use crate::model::JobSpec;
use crate::naming::job_name_slug;
use crate::{EngineKind, ExecutorConfig};
use anyhow::{Result, anyhow};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tokio::fs as tokio_fs;

const MACOS_SRT_COMPAT_RUNNER_JS: &str = include_str!("sandbox_macos_compat_runner.mjs");

#[derive(Debug, Default)]
pub(crate) struct ResolvedSandboxRuntime {
    pub settings_path: Option<PathBuf>,
    pub debug: bool,
}

#[derive(Debug, Clone)]
pub struct SandboxExecutor {
    core: ExecutorCore,
}

impl SandboxExecutor {
    pub async fn new(mut config: ExecutorConfig) -> Result<Self> {
        config.engine = EngineKind::Sandbox;
        let core = ExecutorCore::new(config).await?;
        Ok(Self { core })
    }

    pub async fn run(&self) -> ExecutionOutcome {
        self.core.run().await
    }

    pub fn build_command(
        ctx: &EngineCommandContext<'_>,
        settings: Option<&Path>,
        debug: bool,
    ) -> Command {
        let mut command = if should_use_macos_srt_compat(settings) {
            let mut compat = Command::new("node");
            compat
                .arg("--input-type=module")
                .arg("--eval")
                .arg(MACOS_SRT_COMPAT_RUNNER_JS)
                .arg(settings.expect("settings path is required when using macOS compat"))
                .arg(ctx.container_script);
            if debug {
                compat.env("SRT_DEBUG", "1");
            }
            compat
        } else {
            let mut srt = Command::new("srt");
            if debug {
                srt.arg("--debug");
            }
            if let Some(settings) = settings {
                srt.arg("--settings").arg(settings);
            }
            srt.arg("sh").arg(ctx.container_script);
            srt
        };

        command
            .current_dir(ctx.workdir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in ctx.env_vars {
            command.env(key, value);
        }
        command
    }
}

fn should_use_macos_srt_compat(settings: Option<&Path>) -> bool {
    should_use_macos_srt_compat_impl(
        cfg!(target_os = "macos"),
        settings.is_some(),
        std::env::var("OPAL_SANDBOX_DISABLE_APPLE_CONTAINER_COMPAT").ok(),
    )
}

fn should_use_macos_srt_compat_impl(
    is_macos: bool,
    has_settings: bool,
    disable_env: Option<String>,
) -> bool {
    if !is_macos || !has_settings {
        return false;
    }
    !is_truthy(disable_env.as_deref())
}

fn is_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

pub(crate) async fn prepare_job_env(
    container_workdir: &Path,
    inherited_ci_project_dir: Option<&str>,
    host_workdir: &Path,
    env_vars: &mut Vec<(String, String)>,
) -> Result<()> {
    let sandbox_project_dir = host_workdir.display().to_string();
    rewrite_env_prefix(
        env_vars,
        &container_workdir.display().to_string(),
        &sandbox_project_dir,
    );
    if let Some(inherited_ci_project_dir) = inherited_ci_project_dir
        && !inherited_ci_project_dir.is_empty()
        && inherited_ci_project_dir != container_workdir.display().to_string()
    {
        rewrite_env_prefix(env_vars, inherited_ci_project_dir, &sandbox_project_dir);
    }
    upsert_env_var(env_vars, "CI_PROJECT_DIR", sandbox_project_dir);

    let sandbox_tmp_dir = host_workdir.join(".tmp");
    tokio_fs::create_dir_all(&sandbox_tmp_dir)
        .await
        .map_err(|err| {
            anyhow!(
                "failed to create sandbox tmp dir {}: {err}",
                sandbox_tmp_dir.display()
            )
        })?;
    let srt_tmp_dir = Path::new("/tmp/claude");
    tokio_fs::create_dir_all(srt_tmp_dir).await.map_err(|err| {
        anyhow!(
            "failed to create sandbox runtime tmp dir {}: {err}",
            srt_tmp_dir.display()
        )
    })?;
    let sandbox_tmp_dir = sandbox_tmp_dir.display().to_string();
    upsert_env_var(env_vars, "TMPDIR", sandbox_tmp_dir.clone());
    upsert_env_var(env_vars, "TMP", sandbox_tmp_dir.clone());
    upsert_env_var(env_vars, "TEMP", sandbox_tmp_dir);
    Ok(())
}

pub(crate) async fn resolve_runtime(
    session_dir: &Path,
    global_cfg: Option<&SandboxEngineConfig>,
    job: &JobSpec,
    override_cfg: Option<&ResolvedJobOverride>,
) -> Result<ResolvedSandboxRuntime> {
    let mut settings = global_cfg.and_then(|cfg| cfg.settings.clone());
    let mut debug = global_cfg.and_then(|cfg| cfg.debug).unwrap_or(false);
    let mut allowed_domains = global_cfg
        .map(|cfg| cfg.allowed_domains.clone())
        .unwrap_or_default();
    let mut denied_domains = global_cfg
        .map(|cfg| cfg.denied_domains.clone())
        .unwrap_or_default();
    let mut allow_unix_sockets = global_cfg
        .map(|cfg| cfg.allow_unix_sockets.clone())
        .unwrap_or_default();
    let mut allow_all_unix_sockets = global_cfg.and_then(|cfg| cfg.allow_all_unix_sockets);
    let mut allow_local_binding = global_cfg.and_then(|cfg| cfg.allow_local_binding);
    let mut deny_read = global_cfg
        .map(|cfg| cfg.deny_read.clone())
        .unwrap_or_default();
    let mut allow_read = global_cfg
        .map(|cfg| cfg.allow_read.clone())
        .unwrap_or_default();
    let mut allow_write = global_cfg
        .map(|cfg| cfg.allow_write.clone())
        .unwrap_or_default();
    let mut deny_write = global_cfg
        .map(|cfg| cfg.deny_write.clone())
        .unwrap_or_default();
    let mut ignore_violations = global_cfg
        .map(|cfg| cfg.ignore_violations.clone())
        .unwrap_or_default();
    let mut enable_weaker_nested_sandbox =
        global_cfg.and_then(|cfg| cfg.enable_weaker_nested_sandbox);
    let mut mandatory_deny_search_depth =
        global_cfg.and_then(|cfg| cfg.mandatory_deny_search_depth);

    if let Some(override_cfg) = override_cfg {
        if let Some(value) = &override_cfg.sandbox_settings {
            settings = Some(value.clone());
        }
        if let Some(value) = override_cfg.sandbox_debug {
            debug = value;
        }
        if !override_cfg.sandbox_allowed_domains.is_empty() {
            allowed_domains = override_cfg.sandbox_allowed_domains.clone();
        }
        if !override_cfg.sandbox_denied_domains.is_empty() {
            denied_domains = override_cfg.sandbox_denied_domains.clone();
        }
        if !override_cfg.sandbox_allow_unix_sockets.is_empty() {
            allow_unix_sockets = override_cfg.sandbox_allow_unix_sockets.clone();
        }
        if let Some(value) = override_cfg.sandbox_allow_all_unix_sockets {
            allow_all_unix_sockets = Some(value);
        }
        if let Some(value) = override_cfg.sandbox_allow_local_binding {
            allow_local_binding = Some(value);
        }
        if !override_cfg.sandbox_deny_read.is_empty() {
            deny_read = override_cfg.sandbox_deny_read.clone();
        }
        if !override_cfg.sandbox_allow_read.is_empty() {
            allow_read = override_cfg.sandbox_allow_read.clone();
        }
        if !override_cfg.sandbox_allow_write.is_empty() {
            allow_write = override_cfg.sandbox_allow_write.clone();
        }
        if !override_cfg.sandbox_deny_write.is_empty() {
            deny_write = override_cfg.sandbox_deny_write.clone();
        }
        if !override_cfg.sandbox_ignore_violations.is_empty() {
            ignore_violations = override_cfg.sandbox_ignore_violations.clone();
        }
        if let Some(value) = override_cfg.sandbox_enable_weaker_nested_sandbox {
            enable_weaker_nested_sandbox = Some(value);
        }
        if let Some(value) = override_cfg.sandbox_mandatory_deny_search_depth {
            mandatory_deny_search_depth = Some(value);
        }
    }
    apply_default_ignore_violations(session_dir, job, &mut ignore_violations);

    let has_inline_settings = !allowed_domains.is_empty()
        || !denied_domains.is_empty()
        || !allow_unix_sockets.is_empty()
        || allow_all_unix_sockets.is_some()
        || allow_local_binding.is_some()
        || !deny_read.is_empty()
        || !allow_read.is_empty()
        || !allow_write.is_empty()
        || !deny_write.is_empty()
        || !ignore_violations.is_empty()
        || enable_weaker_nested_sandbox.is_some()
        || mandatory_deny_search_depth.is_some();

    if settings.is_some() && has_inline_settings {
        return Err(anyhow!(
            "job '{}' cannot combine sandbox_settings with inline sandbox config fields",
            job.name
        ));
    }

    if has_inline_settings {
        let dir = session_dir
            .join(job_name_slug(&job.name))
            .join("sandbox-runtime");
        tokio_fs::create_dir_all(&dir).await.map_err(|err| {
            anyhow!(
                "failed to create sandbox settings dir {}: {err}",
                dir.display()
            )
        })?;
        let path = dir.join("generated-settings.json");
        let mut payload = Map::new();

        let mut network = Map::new();
        network.insert(
            "allowedDomains".to_string(),
            serde_json::to_value(allowed_domains)?,
        );
        network.insert(
            "deniedDomains".to_string(),
            serde_json::to_value(denied_domains)?,
        );
        if !allow_unix_sockets.is_empty() {
            network.insert(
                "allowUnixSockets".to_string(),
                serde_json::to_value(allow_unix_sockets)?,
            );
        }
        if let Some(value) = allow_all_unix_sockets {
            network.insert("allowAllUnixSockets".to_string(), Value::Bool(value));
        }
        if let Some(value) = allow_local_binding {
            network.insert("allowLocalBinding".to_string(), Value::Bool(value));
        }
        payload.insert("network".to_string(), Value::Object(network));

        let mut filesystem = Map::new();
        filesystem.insert("denyRead".to_string(), serde_json::to_value(deny_read)?);
        filesystem.insert("allowRead".to_string(), serde_json::to_value(allow_read)?);
        filesystem.insert("allowWrite".to_string(), serde_json::to_value(allow_write)?);
        filesystem.insert("denyWrite".to_string(), serde_json::to_value(deny_write)?);
        payload.insert("filesystem".to_string(), Value::Object(filesystem));
        if !ignore_violations.is_empty() {
            payload.insert(
                "ignoreViolations".to_string(),
                serde_json::to_value(ignore_violations)?,
            );
        }
        if let Some(value) = enable_weaker_nested_sandbox {
            payload.insert("enableWeakerNestedSandbox".to_string(), Value::Bool(value));
        }
        if let Some(value) = mandatory_deny_search_depth {
            payload.insert(
                "mandatoryDenySearchDepth".to_string(),
                Value::Number(serde_json::Number::from(value)),
            );
        }

        tokio_fs::write(&path, serde_json::to_vec_pretty(&Value::Object(payload))?)
            .await
            .map_err(|err| anyhow!("failed to write sandbox settings {}: {err}", path.display()))?;
        return Ok(ResolvedSandboxRuntime {
            settings_path: Some(path),
            debug,
        });
    }

    Ok(ResolvedSandboxRuntime {
        settings_path: settings.map(PathBuf::from),
        debug,
    })
}

fn apply_default_ignore_violations(
    session_dir: &Path,
    job: &JobSpec,
    ignore_violations: &mut BTreeMap<String, Vec<String>>,
) {
    let cargo_registry_src = session_dir
        .join("workspaces")
        .join(job_name_slug(&job.name))
        .join(".cargo")
        .join("registry")
        .join("src")
        .display()
        .to_string();
    let cargo_registry_src_recursive = format!("{cargo_registry_src}/**");
    let cargo_registry_gitmodules = format!("{cargo_registry_src}/**/.gitmodules");
    for key in ["*", "cargo", "cargo install", "cargo test"] {
        let paths = ignore_violations.entry(key.to_string()).or_default();
        if !paths.iter().any(|path| path == &cargo_registry_src) {
            paths.push(cargo_registry_src.clone());
        }
        if !paths
            .iter()
            .any(|path| path == &cargo_registry_src_recursive)
        {
            paths.push(cargo_registry_src_recursive.clone());
        }
        if !paths.iter().any(|path| path == &cargo_registry_gitmodules) {
            paths.push(cargo_registry_gitmodules.clone());
        }
    }
}

fn upsert_env_var(env: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some((_, existing)) = env.iter_mut().find(|(existing, _)| existing == key) {
        *existing = value;
    } else {
        env.push((key.to_string(), value));
    }
}

fn rewrite_env_prefix(env: &mut [(String, String)], from: &str, to: &str) {
    for (_, value) in env.iter_mut() {
        if value == from {
            *value = to.to_string();
            continue;
        }
        if let Some(suffix) = value.strip_prefix(from) {
            *value = format!("{to}{suffix}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_truthy, should_use_macos_srt_compat_impl};

    #[test]
    fn truthy_env_values_are_recognized() {
        assert!(is_truthy(Some("1")));
        assert!(is_truthy(Some("true")));
        assert!(is_truthy(Some("YES")));
        assert!(is_truthy(Some("on")));
        assert!(!is_truthy(Some("0")));
        assert!(!is_truthy(Some("false")));
        assert!(!is_truthy(None));
    }

    #[test]
    fn macos_compat_requires_macos_and_settings() {
        assert!(!should_use_macos_srt_compat_impl(false, true, None));
        assert!(!should_use_macos_srt_compat_impl(true, false, None));
        assert!(should_use_macos_srt_compat_impl(true, true, None));
    }

    #[test]
    fn macos_compat_can_be_disabled_by_env() {
        assert!(!should_use_macos_srt_compat_impl(
            true,
            true,
            Some("1".to_string())
        ));
        assert!(!should_use_macos_srt_compat_impl(
            true,
            true,
            Some("true".to_string())
        ));
        assert!(should_use_macos_srt_compat_impl(
            true,
            true,
            Some("0".to_string())
        ));
    }
}
