use crate::EngineKind;
use crate::executor::core::ExecutorCore;
use crate::naming::job_name_slug;
use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::fmt::Write as _;
use std::fs;
use std::process::Command;

pub(super) fn write_runtime_summary(
    exec: &ExecutorCore,
    job_name: &str,
    container_name: &str,
    job_engine: EngineKind,
    service_engine: EngineKind,
    service_network: Option<&str>,
    service_containers: &[String],
) -> Result<Option<String>> {
    let mut lines = Vec::new();
    lines.push(format!("Job: {job_name}"));
    lines.push(format!("Engine: {}", engine_name(job_engine)));
    if !service_containers.is_empty() {
        lines.push(format!("Service engine: {}", engine_name(service_engine)));
    }
    lines.push(String::new());

    lines.push("Main container".to_string());
    lines.push(format_container_summary(
        job_engine,
        container_name,
        service_network,
    )?);

    if let Some(network) = service_network {
        lines.push(String::new());
        lines.push(format!("Service network: {network}"));
    }

    if !service_containers.is_empty() {
        lines.push(String::new());
        lines.push("Service containers".to_string());
        for name in service_containers {
            lines.push(format_container_summary(
                service_engine,
                name,
                service_network,
            )?);
        }
    }

    let runtime_dir = exec
        .session_dir
        .join(job_name_slug(job_name))
        .join("runtime");
    fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("failed to create {}", runtime_dir.display()))?;
    let summary_path = runtime_dir.join("inspect.txt");
    fs::write(&summary_path, lines.join("\n\n"))
        .with_context(|| format!("failed to write {}", summary_path.display()))?;
    Ok(Some(summary_path.display().to_string()))
}

fn format_container_summary(
    engine: EngineKind,
    container_name: &str,
    service_network: Option<&str>,
) -> Result<String> {
    let mut out = String::new();
    writeln!(&mut out, "- name: {container_name}")?;
    let value = match inspect_container(engine, container_name) {
        Ok(value) => value,
        Err(err) => {
            // Runtime metadata should not flip a job result to failed; keep best-effort diagnostics.
            writeln!(&mut out, "  inspect_error: {err}")?;
            return Ok(out.trim_end().to_string());
        }
    };
    if let Some(image) =
        extract_string(&value, &["Config", "Image"]).or_else(|| extract_string(&value, &["image"]))
    {
        writeln!(&mut out, "  image: {image}")?;
    }
    if let Some(status) =
        extract_string(&value, &["State", "Status"]).or_else(|| extract_string(&value, &["status"]))
    {
        writeln!(&mut out, "  status: {status}")?;
    }
    if let Some(health) = extract_string(&value, &["State", "Health", "Status"])
        .or_else(|| extract_string(&value, &["health", "status"]))
    {
        writeln!(&mut out, "  health: {health}")?;
    }
    if let Some(exit_code) =
        extract_i64(&value, &["State", "ExitCode"]).or_else(|| extract_i64(&value, &["exitCode"]))
    {
        writeln!(&mut out, "  exit_code: {exit_code}")?;
    }
    if let Some(started) = extract_string(&value, &["State", "StartedAt"])
        .or_else(|| extract_string(&value, &["startedAt"]))
    {
        writeln!(&mut out, "  started_at: {started}")?;
    }
    if let Some(finished) = extract_string(&value, &["State", "FinishedAt"])
        .or_else(|| extract_string(&value, &["finishedAt"]))
    {
        writeln!(&mut out, "  finished_at: {finished}")?;
    }

    let networks = extract_networks(&value, service_network);
    if !networks.is_empty() {
        writeln!(&mut out, "  networks:")?;
        for network in networks {
            writeln!(&mut out, "    - {network}")?;
        }
    }

    let mounts = extract_mounts(&value);
    if !mounts.is_empty() {
        writeln!(&mut out, "  mounts:")?;
        for mount in mounts {
            writeln!(&mut out, "    - {mount}")?;
        }
    }
    Ok(out.trim_end().to_string())
}

fn inspect_container(engine: EngineKind, container_name: &str) -> Result<Value> {
    let output = Command::new(engine_binary(engine))
        .arg("inspect")
        .arg(container_name)
        .output()
        .with_context(|| format!("failed to inspect container '{container_name}'"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "inspect failed for container '{}' with status {:?}",
            container_name,
            output.status.code()
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("failed to parse inspect output for '{container_name}'"))?;
    Ok(value
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or(value))
}

fn extract_string<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str()
}

fn extract_i64(value: &Value, path: &[&str]) -> Option<i64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_i64()
}

fn extract_networks(value: &Value, preferred_network: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(networks) = value
        .get("NetworkSettings")
        .and_then(|settings| settings.get("Networks"))
        .and_then(|networks| networks.as_object())
    {
        for (name, network) in networks {
            let ip = network
                .get("IPAddress")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let aliases = network
                .get("Aliases")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let mut line = if ip.is_empty() {
                name.clone()
            } else {
                format!("{name} ({ip})")
            };
            if !aliases.is_empty() {
                line.push_str(&format!(" aliases: {aliases}"));
            }
            out.push(line);
        }
    } else if let Some(items) = value.get("networks").and_then(|v| v.as_array()) {
        for network in items {
            let ip = network
                .get("ipv4Address")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .split('/')
                .next()
                .unwrap_or("");
            let name = preferred_network.unwrap_or("network");
            if ip.is_empty() {
                out.push(name.to_string());
            } else {
                out.push(format!("{name} ({ip})"));
            }
        }
    }
    out
}

fn extract_mounts(value: &Value) -> Vec<String> {
    value
        .get("Mounts")
        .and_then(|v| v.as_array())
        .map(|mounts| {
            mounts
                .iter()
                .filter_map(|mount| {
                    let source = mount.get("Source").and_then(|v| v.as_str())?;
                    let dest = mount
                        .get("Destination")
                        .or_else(|| mount.get("Target"))
                        .and_then(|v| v.as_str())?;
                    Some(format!("{source} -> {dest}"))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn engine_binary(engine: EngineKind) -> &'static str {
    match engine {
        EngineKind::ContainerCli => "container",
        EngineKind::Docker | EngineKind::Orbstack => "docker",
        EngineKind::Podman => "podman",
        EngineKind::Nerdctl => "nerdctl",
        EngineKind::Sandbox => "srt",
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
    use super::format_container_summary;
    use crate::EngineKind;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn format_container_summary_is_best_effort_when_inspect_fails() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough for test")
            .as_nanos();
        let container_name = format!("opal-runtime-summary-missing-{nanos}");

        let summary = format_container_summary(EngineKind::Docker, &container_name, None)
            .expect("runtime summary formatting should not fail on inspect errors");

        assert!(summary.contains(&format!("- name: {container_name}")));
        assert!(summary.contains("inspect_error:"));
    }
}
