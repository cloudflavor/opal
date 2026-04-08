use super::ExecutorCore;
use crate::pipeline::VolumeMount;
use anyhow::{Context, Result, anyhow};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

pub(super) async fn apply_runner_bootstrap(exec: &mut ExecutorCore) -> Result<()> {
    let bootstrap = exec.config.settings.bootstrap_settings().clone();
    if !bootstrap.is_active() {
        return Ok(());
    }

    if let Some(command) = bootstrap.command.as_deref() {
        run_bootstrap_command(command, &exec.config.workdir, &exec.shared_env).await?;
    }

    if let Some(env_file) = bootstrap.env_file.as_ref() {
        let env_from_file = crate::secrets::load_dotenv_env_pairs_async(env_file)
            .await
            .with_context(|| {
                format!(
                    "failed to load bootstrap env file at {}",
                    env_file.display()
                )
            })?;
        for (key, value) in env_from_file {
            upsert_executor_env(exec, &key, &value);
        }
    }

    let mut lookup = exec.shared_env.clone();
    for (key, raw_value) in &bootstrap.env {
        let expanded = crate::env::expand_value(raw_value, &lookup);
        upsert_executor_env(exec, key, &expanded);
        lookup.insert(key.clone(), expanded);
    }

    for mount in &bootstrap.mounts {
        if mount.host.as_os_str().is_empty() {
            return Err(anyhow!("bootstrap mount host path must not be empty"));
        }
        if mount.container.as_os_str().is_empty() {
            return Err(anyhow!("bootstrap mount container path must not be empty"));
        }
        if !mount.container.is_absolute() {
            return Err(anyhow!(
                "bootstrap mount container path '{}' must be absolute",
                mount.container.display()
            ));
        }
        if !mount.host.exists() {
            return Err(anyhow!(
                "bootstrap mount host path '{}' does not exist",
                mount.host.display()
            ));
        }
        exec.bootstrap_mounts.push(VolumeMount {
            host: mount.host.clone(),
            container: mount.container.clone(),
            read_only: mount.read_only,
        });
    }

    Ok(())
}

async fn run_bootstrap_command(
    command: &str,
    workdir: &Path,
    env: &std::collections::HashMap<String, String>,
) -> Result<()> {
    let status = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .current_dir(workdir)
        .envs(env)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .with_context(|| format!("failed to run bootstrap command '{command}'"))?;

    if !status.success() {
        return Err(anyhow!(
            "bootstrap command '{command}' failed with status {:?}",
            status.code()
        ));
    }
    Ok(())
}

fn upsert_executor_env(exec: &mut ExecutorCore, key: &str, value: &str) {
    if let Some((_, existing)) = exec
        .env_vars
        .iter_mut()
        .find(|(existing_key, _)| existing_key == key)
    {
        *existing = value.to_string();
    } else {
        exec.env_vars.push((key.to_string(), value.to_string()));
    }
    exec.shared_env.insert(key.to_string(), value.to_string());
}
