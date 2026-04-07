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
        Ok(unique_ports(
            infos
                .into_iter()
                .flat_map(|info| info.variants)
                .flat_map(|variant| variant.config.history)
                .flat_map(|entry| ports_from_history_entry(&entry)),
        ))
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

    if let Some(ip) = container_cli_ipv4(service) {
        return Ok(ip.split('/').next().map(str::to_string));
    }

    if let Some(ip) = docker_ipv4(service) {
        return Ok(Some(ip.to_string()));
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

    let status = lowercase_state_field(state, "Status", "status");
    let health = nested_lowercase_state_field(state, "Health", "Status", "health", "status");
    let exit_code = i64_state_field(state, "ExitCode", "exitCode");
    let running = bool_state_field(state, "Running", "running")
        .unwrap_or_else(|| status.as_deref().is_some_and(|status| status == "running"));

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

fn unique_ports(ports: impl IntoIterator<Item = ServicePort>) -> Vec<ServicePort> {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for port in ports {
        if seen.insert((port.port, port.proto.clone())) {
            unique.push(port);
        }
    }
    unique
}

fn container_cli_ipv4(service: &Value) -> Option<&str> {
    service
        .get("networks")
        .and_then(|networks| networks.as_array())
        .and_then(|items| items.first())
        .and_then(|network| network.get("ipv4Address"))
        .and_then(|value| value.as_str())
}

fn docker_ipv4(service: &Value) -> Option<&str> {
    service
        .get("NetworkSettings")
        .and_then(|settings| settings.get("Networks"))
        .and_then(|networks| networks.as_object())
        .into_iter()
        .flat_map(|networks| networks.values())
        .filter_map(|network| network.get("IPAddress").and_then(|value| value.as_str()))
        .find(|ip| !ip.is_empty())
}

fn bool_state_field(state: &Value, primary: &str, fallback: &str) -> Option<bool> {
    state
        .get(primary)
        .and_then(|value| value.as_bool())
        .or_else(|| state.get(fallback).and_then(|value| value.as_bool()))
}

fn lowercase_state_field(state: &Value, primary: &str, fallback: &str) -> Option<String> {
    state
        .get(primary)
        .and_then(|value| value.as_str())
        .or_else(|| state.get(fallback).and_then(|value| value.as_str()))
        .map(|value| value.to_ascii_lowercase())
}

fn nested_lowercase_state_field(
    state: &Value,
    primary_parent: &str,
    primary_child: &str,
    fallback_parent: &str,
    fallback_child: &str,
) -> Option<String> {
    state
        .get(primary_parent)
        .and_then(|value| value.get(primary_child))
        .and_then(|value| value.as_str())
        .or_else(|| {
            state
                .get(fallback_parent)
                .and_then(|value| value.get(fallback_child))
                .and_then(|value| value.as_str())
        })
        .map(|value| value.to_ascii_lowercase())
}

fn i64_state_field(state: &Value, primary: &str, fallback: &str) -> Option<i64> {
    state
        .get(primary)
        .and_then(|value| value.as_i64())
        .or_else(|| state.get(fallback).and_then(|value| value.as_i64()))
}
