use crate::EngineKind;
use crate::env::expand_env_list;
use crate::model::ServiceSpec;
use crate::naming::job_name_slug;
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as FmtWrite;
use std::process::Command;
use std::thread;
use std::time::Duration;
use tracing::warn;

const MAX_NAME_LEN: usize = 63;
const CONTAINER_NETWORK_RETRY_ATTEMPTS: usize = 3;
const CONTAINER_NETWORK_RETRY_DELAY_MS: u64 = 500;

pub struct ServiceRuntime {
    engine: EngineKind,
    network: String,
    containers: Vec<String>,
    link_env: Vec<(String, String)>,
}

impl ServiceRuntime {
    pub fn start(
        engine: EngineKind,
        run_id: &str,
        job_name: &str,
        services: &[ServiceSpec],
        base_env: &[(String, String)],
        shared_env: &HashMap<String, String>,
    ) -> Result<Option<Self>> {
        if services.is_empty() {
            return Ok(None);
        }
        service_supported(engine)?;
        let clean_run_id = sanitize_identifier(run_id);
        let network = clamp_name(&format!(
            "opal-net-{}-{}",
            clean_run_id,
            job_name_slug(job_name)
        ));
        run_network_create(engine, &network)
            .with_context(|| format!("failed to create network {}", network))?;

        let mut runtime = ServiceRuntime {
            engine,
            network: network.clone(),
            containers: Vec::new(),
            link_env: Vec::new(),
        };

        for (idx, service) in services.iter().enumerate() {
            let raw_alias = service
                .alias
                .clone()
                .unwrap_or_else(|| default_service_alias(&service.image));
            let alias = normalize_alias(&raw_alias);
            let container_name = clamp_name(&format!(
                "opal-svc-{}-{}-{:02}",
                clean_run_id,
                job_name_slug(job_name),
                idx
            ));
            let ports = if matches!(engine, EngineKind::ContainerCli) {
                match discover_container_ports(&service.image) {
                    Ok(list) => list,
                    Err(err) => {
                        warn!(
                            image = %service.image,
                            "failed to detect exposed ports for service: {err}"
                        );
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            if let Err(err) =
                runtime.start_service(&container_name, service, &alias, base_env, shared_env)
            {
                runtime.cleanup();
                return Err(err);
            }
            if matches!(engine, EngineKind::ContainerCli) && !ports.is_empty() {
                runtime
                    .link_env
                    .extend(build_service_env(&alias, &container_name, &ports));
            }
        }

        Ok(Some(runtime))
    }

    pub fn network_name(&self) -> &str {
        &self.network
    }

    pub fn cleanup(&mut self) {
        for name in self.containers.drain(..).rev() {
            let _ = Command::new(engine_binary(self.engine))
                .arg("rm")
                .arg("-f")
                .arg(&name)
                .status();
        }
        let _ = run_network_remove(self.engine, &self.network);
    }

    pub fn link_env(&self) -> &[(String, String)] {
        &self.link_env
    }

    fn start_service(
        &mut self,
        container_name: &str,
        service: &ServiceSpec,
        alias: &str,
        base_env: &[(String, String)],
        shared_env: &HashMap<String, String>,
    ) -> Result<()> {
        let mut command = service_command(self.engine);
        command
            .arg("-d")
            .arg("--rm")
            .arg("--name")
            .arg(container_name)
            .arg("--network")
            .arg(&self.network);
        if !matches!(self.engine, EngineKind::ContainerCli) {
            command.arg("--network-alias").arg(alias);
        }

        let mut merged = merged_env(base_env, &service.variables);
        expand_env_list(&mut merged[..], shared_env);
        for (key, value) in merged {
            command.arg("--env").arg(format!("{key}={value}"));
        }

        if !service.entrypoint.is_empty() {
            command
                .arg("--entrypoint")
                .arg(service.entrypoint.join(" "));
        }

        command.arg(&service.image);

        if matches!(self.engine, EngineKind::ContainerCli) && !service.command.is_empty() {
            let joined = service.command.join(" ");
            command.arg("sh").arg("-c").arg(joined);
        } else {
            for arg in &service.command {
                command.arg(arg);
            }
        }

        if env::var("OPAL_DEBUG_CONTAINER")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            let program = command.get_program().to_string_lossy();
            let args: Vec<String> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect();
            eprintln!("[opal] service command: {} {}", program, args.join(" "));
        }

        run_command(command).with_context(|| {
            format!(
                "failed to start service '{}' ({})",
                container_name, service.image
            )
        })?;
        self.containers.push(container_name.to_string());
        Ok(())
    }
}

fn service_supported(engine: EngineKind) -> Result<()> {
    if matches!(
        engine,
        EngineKind::Docker
            | EngineKind::Orbstack
            | EngineKind::Podman
            | EngineKind::Nerdctl
            | EngineKind::ContainerCli
    ) {
        Ok(())
    } else {
        Err(anyhow!(
            "services are only supported when using docker, podman, nerdctl, or orbstack"
        ))
    }
}

fn engine_binary(engine: EngineKind) -> &'static str {
    match engine {
        EngineKind::Docker | EngineKind::Orbstack => "docker",
        EngineKind::Podman => "podman",
        EngineKind::Nerdctl => "nerdctl",
        EngineKind::ContainerCli => "container",
    }
}

fn service_command(engine: EngineKind) -> Command {
    let mut command = Command::new(engine_binary(engine));
    command.arg("run");
    if matches!(engine, EngineKind::ContainerCli) {
        command.arg("--arch").arg("x86_64");
    }
    command
}

fn sanitize_identifier(input: &str) -> String {
    let filtered: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if filtered.is_empty() {
        "opal".to_string()
    } else {
        filtered
    }
}

fn clamp_name(base: &str) -> String {
    if base.len() <= MAX_NAME_LEN {
        return base.to_string();
    }
    let mut hasher = Sha256::new();
    hasher.update(base.as_bytes());
    let digest = hasher.finalize();
    let mut suffix = String::with_capacity(8);
    for byte in digest.iter().take(4) {
        let _ = FmtWrite::write_fmt(&mut suffix, format_args!("{:02x}", byte));
    }
    let prefix_len = MAX_NAME_LEN.saturating_sub(suffix.len() + 1);
    let prefix: String = base.chars().take(prefix_len).collect();
    format!("{prefix}-{suffix}")
}

fn normalize_alias(alias: &str) -> String {
    let mut normalized = String::new();
    for ch in alias.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' {
            normalized.push('-');
        }
    }
    if normalized.is_empty() {
        "service".to_string()
    } else {
        normalized
    }
}

#[derive(Debug, Clone)]
struct ServicePort {
    port: u16,
    proto: String,
}

fn discover_container_ports(image: &str) -> Result<Vec<ServicePort>> {
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
                if let Some(cmd) = entry.created_by
                    && let Some(idx) = cmd.find("EXPOSE map[")
                {
                    let rest = &cmd[idx + "EXPOSE map[".len()..];
                    if let Some(end) = rest.find(']') {
                        let map = &rest[..end];
                        for token in map.split_whitespace() {
                            let cleaned = token.trim_matches(|c| c == ',' || c == '{' || c == '}');
                            if cleaned.is_empty() {
                                continue;
                            }
                            let mut parts = cleaned.split('/');
                            let port_str = parts.next().unwrap_or("");
                            let proto_part = parts.next().unwrap_or("tcp");
                            if let Ok(port) = port_str.parse::<u16>() {
                                let proto = proto_part
                                    .split(':')
                                    .next()
                                    .unwrap_or("tcp")
                                    .to_ascii_lowercase();
                                if seen.insert((port, proto.clone())) {
                                    ports.push(ServicePort { port, proto });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(ports)
}

fn build_service_env(alias: &str, host: &str, ports: &[ServicePort]) -> Vec<(String, String)> {
    if ports.is_empty() {
        return Vec::new();
    }
    let mut envs = Vec::new();
    let alias_key: String = alias
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let primary = &ports[0];
    envs.push((
        format!("{}_PORT", alias_key),
        format!("{}://{}:{}", primary.proto, host, primary.port),
    ));
    for port in ports {
        let proto_upper = port.proto.to_ascii_uppercase();
        let proto_lower = port.proto.to_ascii_lowercase();
        let base = format!("{}_PORT_{}_{}", alias_key, port.port, proto_upper);
        envs.push((
            base.clone(),
            format!("{}://{}:{}", proto_lower, host, port.port),
        ));
        envs.push((format!("{}_ADDR", base), host.to_string()));
        envs.push((format!("{}_PORT", base), port.port.to_string()));
        envs.push((format!("{}_PROTO", base), proto_lower));
    }
    envs
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

fn run_command(mut cmd: Command) -> Result<()> {
    let output = cmd.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failed(
            &cmd,
            &output.stdout,
            &output.stderr,
            output.status.code(),
        ))
    }
}

fn command_failed(cmd: &Command, stdout: &[u8], stderr: &[u8], code: Option<i32>) -> anyhow::Error {
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let detail = match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(": {stdout}"),
        (true, false) => format!(": {stderr}"),
        (false, false) => format!(": stdout={stdout}; stderr={stderr}"),
    };
    anyhow!("command {:?} exited with status {:?}{detail}", cmd, code)
}

fn run_network_create(engine: EngineKind, network: &str) -> Result<()> {
    run_network_command(engine, "create", network)
}

fn run_network_remove(engine: EngineKind, network: &str) -> Result<()> {
    run_network_command(engine, "rm", network)
}

fn run_network_command(engine: EngineKind, action: &str, network: &str) -> Result<()> {
    let attempts = if matches!(engine, EngineKind::ContainerCli) {
        CONTAINER_NETWORK_RETRY_ATTEMPTS
    } else {
        1
    };

    let mut last_error = None;
    for attempt in 0..attempts {
        let mut command = Command::new(engine_binary(engine));
        command.arg("network").arg(action).arg(network);
        match run_command(command) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if matches!(engine, EngineKind::ContainerCli)
                    && should_retry_container_network_error(&err.to_string())
                    && attempt + 1 < attempts
                {
                    warn!(
                        network,
                        action,
                        attempt = attempt + 1,
                        "container network command timed out; retrying"
                    );
                    thread::sleep(Duration::from_millis(
                        CONTAINER_NETWORK_RETRY_DELAY_MS * (attempt + 1) as u64,
                    ));
                    last_error = Some(err);
                    continue;
                }
                return Err(err);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("network command failed without an error")))
}

fn should_retry_container_network_error(message: &str) -> bool {
    message.contains("XPC timeout for request to com.apple.container.apiserver/networkCreate")
        || message
            .contains("XPC timeout for request to com.apple.container.apiserver/networkDelete")
}

fn merged_env(
    base: &[(String, String)],
    overrides: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut map: HashMap<String, String> = base.iter().cloned().collect();
    for (key, value) in overrides {
        map.insert(key.clone(), value.clone());
    }
    map.into_iter().collect()
}

fn default_service_alias(image: &str) -> String {
    image
        .split('/')
        .next_back()
        .and_then(|part| part.split(':').next())
        .map(|segment| segment.replace(|c: char| !c.is_ascii_alphanumeric(), ""))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "service".to_string())
}

#[cfg(test)]
mod tests {
    use super::should_retry_container_network_error;

    #[test]
    fn retries_container_network_xpc_timeouts() {
        assert!(should_retry_container_network_error(
            "XPC timeout for request to com.apple.container.apiserver/networkCreate"
        ));
        assert!(should_retry_container_network_error(
            "XPC timeout for request to com.apple.container.apiserver/networkDelete"
        ));
        assert!(!should_retry_container_network_error(
            "cannot delete subnet with referring containers"
        ));
    }
}
