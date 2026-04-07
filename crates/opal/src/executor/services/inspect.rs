use super::command::{command_failed, engine_binary};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::process::Command;

pub(super) struct ServiceInspector {
    engine: crate::EngineKind,
}

impl ServiceInspector {
    pub(super) fn new(engine: crate::EngineKind) -> Self {
        Self { engine }
    }

    pub(super) fn state(&self, container_name: &str) -> Result<ServiceState> {
        let mut command = Command::new(engine_binary(self.engine));
        command
            .arg("inspect")
            .arg("--format")
            .arg("{{json .State}}")
            .arg(container_name);
        let output = command
            .output()
            .with_context(|| format!("failed to inspect service container '{container_name}'"))?;
        if output.status.success() {
            return parse_service_state(&output.stdout);
        }

        let mut fallback = Command::new(engine_binary(self.engine));
        fallback.arg("inspect").arg(container_name);
        let output = fallback
            .output()
            .with_context(|| format!("failed to inspect service container '{container_name}'"))?;
        if !output.status.success() {
            return Err(command_failed(
                &fallback,
                &output.stdout,
                &output.stderr,
                output.status.code(),
            ));
        }

        parse_service_state(&output.stdout)
    }

    pub(super) fn ipv4(&self, container_name: &str) -> Option<String> {
        let mut command = Command::new(engine_binary(self.engine));
        command.arg("inspect").arg(container_name);
        let output = command.output().ok()?;
        if !output.status.success() {
            return None;
        }
        parse_service_ipv4(&output.stdout).ok().flatten()
    }

    pub(super) fn discover_ports(&self, image: &str) -> Result<Vec<ServicePort>> {
        let output = Command::new("container")
            .arg("image")
            .arg("inspect")
            .arg(image)
            .output()
            .context("failed to inspect container image")?;
        if !output.status.success() {
            return Ok(Vec::new());
        }

        let infos: Vec<ContainerImageInspect> = serde_json::from_slice(&output.stdout)?;
        let mut ports = Vec::new();
        let mut seen = HashSet::new();
        for info in infos {
            for variant in info.variants {
                for entry in variant.config.history {
                    for port in ports_from_history_entry(&entry) {
                        if seen.insert((port.port, port.proto.clone())) {
                            ports.push(port);
                        }
                    }
                }
            }
        }
        Ok(ports)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServiceState {
    pub(super) running: bool,
    pub(super) status: Option<String>,
    pub(super) health: Option<String>,
    pub(super) exit_code: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServicePort {
    pub(super) port: u16,
    pub(super) proto: String,
}

pub(super) fn parse_service_ipv4(payload: &[u8]) -> Result<Option<String>> {
    let value: Value = serde_json::from_slice(payload)
        .context("failed to parse service inspect output as json")?;
    let service = first_service_object(&value)?;

    if let Some(ip) = service
        .get("networks")
        .and_then(|networks| networks.as_array())
        .and_then(|items| items.first())
        .and_then(|network| network.get("ipv4Address"))
        .and_then(|value| value.as_str())
    {
        return Ok(ip.split('/').next().map(str::to_string));
    }

    if let Some(networks) = service
        .get("NetworkSettings")
        .and_then(|settings| settings.get("Networks"))
        .and_then(|networks| networks.as_object())
    {
        for network in networks.values() {
            if let Some(ip) = network.get("IPAddress").and_then(|value| value.as_str())
                && !ip.is_empty()
            {
                return Ok(Some(ip.to_string()));
            }
        }
    }

    Ok(None)
}

pub(super) fn parse_service_state(payload: &[u8]) -> Result<ServiceState> {
    let value: Value = serde_json::from_slice(payload)
        .context("failed to parse service inspect output as json")?;
    let service = first_service_object(&value)?;

    let state = if let Some(state) = service.get("State") {
        state
    } else if service.get("Running").is_some()
        || service.get("Status").is_some()
        || service.get("status").is_some()
    {
        service
    } else {
        return Err(anyhow!("service inspect output missing State field"));
    };

    let running = state
        .get("Running")
        .and_then(|value| value.as_bool())
        .or_else(|| state.get("running").and_then(|value| value.as_bool()))
        .unwrap_or_else(|| {
            state
                .get("Status")
                .and_then(|value| value.as_str())
                .or_else(|| state.get("status").and_then(|value| value.as_str()))
                .is_some_and(|status| status.eq_ignore_ascii_case("running"))
        });
    let status = state
        .get("Status")
        .and_then(|value| value.as_str())
        .or_else(|| state.get("status").and_then(|value| value.as_str()))
        .map(|status| status.to_ascii_lowercase());
    let health = state
        .get("Health")
        .and_then(|health| health.get("Status"))
        .and_then(|status| status.as_str())
        .or_else(|| {
            state
                .get("health")
                .and_then(|health| health.get("status"))
                .and_then(|status| status.as_str())
        })
        .map(|status| status.to_ascii_lowercase());
    let exit_code = state
        .get("ExitCode")
        .and_then(|value| value.as_i64())
        .or_else(|| state.get("exitCode").and_then(|value| value.as_i64()));

    Ok(ServiceState {
        running,
        status,
        health,
        exit_code,
    })
}

#[derive(Deserialize)]
struct ContainerImageInspect {
    variants: Vec<ContainerVariant>,
}

#[derive(Deserialize)]
struct ContainerVariant {
    config: VariantConfig,
}

#[derive(Deserialize)]
struct VariantConfig {
    history: Vec<HistoryEntry>,
}

#[derive(Deserialize)]
struct HistoryEntry {
    #[serde(rename = "created_by")]
    created_by: Option<String>,
}

fn first_service_object(value: &Value) -> Result<&Value> {
    value
        .as_array()
        .and_then(|items| items.first())
        .or_else(|| value.as_object().map(|_| value))
        .ok_or_else(|| anyhow!("service inspect output was not an object or array"))
}

fn ports_from_history_entry(entry: &HistoryEntry) -> Vec<ServicePort> {
    let Some(command) = entry.created_by.as_deref() else {
        return Vec::new();
    };
    let Some(expose_map) = extract_expose_map(command) else {
        return Vec::new();
    };

    expose_map
        .split_whitespace()
        .filter_map(parse_exposed_port)
        .collect()
}

fn extract_expose_map(command: &str) -> Option<&str> {
    let idx = command.find("EXPOSE map[")?;
    let rest = &command[idx + "EXPOSE map[".len()..];
    let end = rest.find(']')?;
    Some(&rest[..end])
}

fn parse_exposed_port(token: &str) -> Option<ServicePort> {
    let cleaned = token.trim_matches(|ch| ch == ',' || ch == '{' || ch == '}');
    if cleaned.is_empty() {
        return None;
    }

    let mut parts = cleaned.split('/');
    let port = parts.next()?.parse::<u16>().ok()?;
    let proto = parts
        .next()
        .unwrap_or("tcp")
        .split(':')
        .next()
        .unwrap_or("tcp")
        .to_ascii_lowercase();
    Some(ServicePort { port, proto })
}
