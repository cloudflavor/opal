use super::ServicePort;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::from_slice as json_from_slice;
use std::collections::HashSet;
use tokio::process::Command;

pub(super) async fn discover_ports(image: &str) -> Result<Vec<ServicePort>> {
    let output = Command::new("container")
        .arg("image")
        .arg("inspect")
        .arg(image)
        .output()
        .await
        .context("failed to inspect container image")?;
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let infos: Vec<ContainerImageInspect> = json_from_slice(&output.stdout)?;
    Ok(unique_ports(
        infos
            .into_iter()
            .flat_map(|info| info.variants)
            .flat_map(|variant| variant.config.history)
            .flat_map(|entry| ports_from_history_entry(&entry)),
    ))
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
