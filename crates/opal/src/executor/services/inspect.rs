mod container_cli;

use super::command::{command_failed, engine_binary};
use crate::EngineKind;
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, from_slice as json_from_slice};
use tokio::process::Command;

pub(super) struct ServiceInspector {
    engine: EngineKind,
}

impl ServiceInspector {
    pub(super) fn new(engine: EngineKind) -> Self {
        Self { engine }
    }

    pub(super) async fn state(&self, container_name: &str) -> Result<ServiceState> {
        let mut command = Command::new(engine_binary(self.engine));
        command
            .arg("inspect")
            .arg("--format")
            .arg("{{json .State}}")
            .arg(container_name);
        let output = command
            .output()
            .await
            .with_context(|| format!("failed to inspect service container '{container_name}'"))?;
        if output.status.success() {
            return parse_service_state(&output.stdout);
        }

        let mut fallback = Command::new(engine_binary(self.engine));
        fallback.arg("inspect").arg(container_name);
        let output = fallback
            .output()
            .await
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

    pub(super) async fn ipv4(&self, container_name: &str) -> Option<String> {
        let mut command = Command::new(engine_binary(self.engine));
        command.arg("inspect").arg(container_name);
        let output = command.output().await.ok()?;
        if !output.status.success() {
            return None;
        }
        parse_service_ipv4(&output.stdout).ok().flatten()
    }

    pub(super) async fn discover_ports(&self, image: &str) -> Result<Vec<ServicePort>> {
        container_cli::discover_ports(image).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServiceState {
    pub(super) running: bool,
    pub(super) status: Option<ServiceContainerStatus>,
    pub(super) health: Option<ServiceHealthStatus>,
    pub(super) exit_code: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServicePort {
    pub(super) port: u16,
    pub(super) proto: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServiceContainerStatus {
    Running,
    Exited,
    Dead,
    Stopped,
    Other(String),
}

impl ServiceContainerStatus {
    fn parse(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "running" => Self::Running,
            "exited" => Self::Exited,
            "dead" => Self::Dead,
            "stopped" => Self::Stopped,
            other => Self::Other(other.to_string()),
        }
    }

    pub(super) fn as_str(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Dead => "dead",
            Self::Stopped => "stopped",
            Self::Other(other) => other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServiceHealthStatus {
    Healthy,
    Unhealthy,
    Other(String),
}

impl ServiceHealthStatus {
    fn parse(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "healthy" => Self::Healthy,
            "unhealthy" => Self::Unhealthy,
            other => Self::Other(other.to_string()),
        }
    }

    pub(super) fn as_str(&self) -> &str {
        match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::Other(other) => other,
        }
    }
}

pub(super) fn parse_service_ipv4(payload: &[u8]) -> Result<Option<String>> {
    let value: Value =
        json_from_slice(payload).context("failed to parse service inspect output as json")?;
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
    let value: Value =
        json_from_slice(payload).context("failed to parse service inspect output as json")?;
    let service = first_service_object(&value)?;

    let state = service_state_value(service)?;

    let status = string_state_field(state, "Status", "status").map(ServiceContainerStatus::parse);
    let health = nested_string_state_field(state, "Health", "Status", "health", "status")
        .map(ServiceHealthStatus::parse);
    let exit_code = i64_state_field(state, "ExitCode", "exitCode");
    let running = bool_state_field(state, "Running", "running")
        .unwrap_or(matches!(status, Some(ServiceContainerStatus::Running)));

    Ok(ServiceState {
        running,
        status,
        health,
        exit_code,
    })
}

fn first_service_object(value: &Value) -> Result<&Value> {
    value
        .as_array()
        .and_then(|items| items.first())
        .or_else(|| value.as_object().map(|_| value))
        .ok_or_else(|| anyhow!("service inspect output was not an object or array"))
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

fn service_state_value(service: &Value) -> Result<&Value> {
    if let Some(state) = service.get("State") {
        return Ok(state);
    }
    if service.get("Running").is_some()
        || service.get("Status").is_some()
        || service.get("status").is_some()
    {
        return Ok(service);
    }
    Err(anyhow!("service inspect output missing State field"))
}

fn bool_state_field(state: &Value, primary: &str, fallback: &str) -> Option<bool> {
    state
        .get(primary)
        .and_then(|value| value.as_bool())
        .or_else(|| state.get(fallback).and_then(|value| value.as_bool()))
}

fn string_state_field<'a>(state: &'a Value, primary: &str, fallback: &str) -> Option<&'a str> {
    state
        .get(primary)
        .and_then(|value| value.as_str())
        .or_else(|| state.get(fallback).and_then(|value| value.as_str()))
}

fn nested_string_state_field<'a>(
    state: &'a Value,
    primary_parent: &str,
    primary_child: &str,
    fallback_parent: &str,
    fallback_child: &str,
) -> Option<&'a str> {
    nested_state_field(state, primary_parent, primary_child)
        .or_else(|| nested_state_field(state, fallback_parent, fallback_child))
}

fn nested_state_field<'a>(state: &'a Value, parent: &str, child: &str) -> Option<&'a str> {
    state
        .get(parent)
        .and_then(|value| value.get(child))
        .and_then(|value| value.as_str())
}

fn i64_state_field(state: &Value, primary: &str, fallback: &str) -> Option<i64> {
    state
        .get(primary)
        .and_then(|value| value.as_i64())
        .or_else(|| state.get(fallback).and_then(|value| value.as_i64()))
}
