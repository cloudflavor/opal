use crate::EngineKind;
use crate::model::ServiceSpec;
use crate::naming::job_name_slug;
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as FmtWrite;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use tracing::warn;

const MAX_NAME_LEN: usize = 63;
const CONTAINER_NETWORK_RETRY_ATTEMPTS: usize = 3;
const CONTAINER_NETWORK_RETRY_DELAY_MS: u64 = 500;
const SERVICE_READY_TIMEOUT_DEFAULT_SECS: u64 = 30;
const SERVICE_READY_POLL_MS: u64 = 250;

pub struct ServiceRuntime {
    engine: EngineKind,
    network: String,
    containers: Vec<String>,
    link_env: Vec<(String, String)>,
    claimed_aliases: HashSet<String>,
    host_aliases: Vec<(String, String)>,
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
            claimed_aliases: HashSet::new(),
            host_aliases: Vec::new(),
        };

        for (idx, service) in services.iter().enumerate() {
            let aliases = runtime.aliases_for_service(idx, service)?;
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
                runtime.start_service(&container_name, service, &aliases, base_env, shared_env)
            {
                runtime.cleanup();
                return Err(err);
            }
            if let Err(err) = runtime.wait_for_service_readiness(&container_name, service, &ports) {
                runtime.cleanup();
                return Err(err);
            }
            if matches!(engine, EngineKind::ContainerCli)
                && let Some(ip) = inspect_service_ipv4(engine, &container_name)
            {
                for alias in &aliases {
                    runtime.host_aliases.push((alias.clone(), ip.clone()));
                }
            }
            if matches!(engine, EngineKind::ContainerCli) && !ports.is_empty() {
                for alias in &aliases {
                    runtime
                        .link_env
                        .extend(build_service_env(alias, &container_name, &ports));
                }
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

    pub fn host_aliases(&self) -> &[(String, String)] {
        &self.host_aliases
    }

    fn start_service(
        &mut self,
        container_name: &str,
        service: &ServiceSpec,
        aliases: &[String],
        base_env: &[(String, String)],
        _shared_env: &HashMap<String, String>,
    ) -> Result<()> {
        let mut command = service_command(self.engine);
        command
            .arg("-d")
            .arg("--name")
            .arg(container_name)
            .arg("--network")
            .arg(&self.network);
        if !matches!(self.engine, EngineKind::ContainerCli) {
            for alias in aliases {
                command.arg("--network-alias").arg(alias);
            }
        }

        let merged = merged_env(base_env, &service.variables);
        for (key, value) in merged {
            command.arg("--env").arg(format!("{key}={value}"));
        }

        if !service.entrypoint.is_empty() {
            command
                .arg("--entrypoint")
                .arg(service.entrypoint.join(" "));
        }

        command.arg(&service.image);

        for arg in &service.command {
            command.arg(arg);
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

    fn aliases_for_service(&mut self, idx: usize, service: &ServiceSpec) -> Result<Vec<String>> {
        let mut accepted = Vec::new();
        if service.aliases.is_empty() {
            for alias in default_service_aliases(&service.image) {
                if self.claimed_aliases.insert(alias.clone()) {
                    accepted.push(alias);
                }
            }
        } else {
            for raw in service.aliases.clone() {
                let alias = validate_service_alias(&raw)?;
                if self.claimed_aliases.insert(alias.clone()) {
                    accepted.push(alias);
                }
            }
        }

        if accepted.is_empty() {
            let fallback = validate_service_alias(&format!("svc-{idx}"))?;
            self.claimed_aliases.insert(fallback.clone());
            accepted.push(fallback);
        }

        Ok(accepted)
    }

    fn wait_for_service_readiness(
        &self,
        container_name: &str,
        service: &ServiceSpec,
        ports: &[ServicePort],
    ) -> Result<()> {
        let timeout = service_ready_timeout();
        let started = Instant::now();
        let mut confirmed_running_without_health = false;

        loop {
            let state = match inspect_service_state(self.engine, container_name) {
                Ok(state) => state,
                Err(err) => {
                    warn!(
                        service = container_name,
                        "failed to inspect service readiness ({err}); continuing without readiness gate"
                    );
                    return Ok(());
                }
            };

            match readiness_from_state(&state) {
                ServiceReadiness::Ready => {
                    if state.health.is_none() {
                        if !ports.is_empty() {
                            let Some(ip) = inspect_service_ipv4(self.engine, container_name) else {
                                if started.elapsed() >= timeout {
                                    return Err(anyhow!(
                                        "service '{}' ({}) did not expose a reachable IP within {}s",
                                        container_name,
                                        service.image,
                                        timeout.as_secs()
                                    ));
                                }
                                thread::sleep(Duration::from_millis(SERVICE_READY_POLL_MS));
                                continue;
                            };
                            if probe_service_ports(self.engine, &self.network, &ip, ports)? {
                                return Ok(());
                            }
                            if started.elapsed() >= timeout {
                                return Err(anyhow!(
                                    "service '{}' ({}) did not accept connections on exposed ports within {}s",
                                    container_name,
                                    service.image,
                                    timeout.as_secs()
                                ));
                            }
                            thread::sleep(Duration::from_millis(SERVICE_READY_POLL_MS));
                            continue;
                        }
                        if !confirmed_running_without_health {
                            confirmed_running_without_health = true;
                            thread::sleep(Duration::from_millis(SERVICE_READY_POLL_MS));
                            continue;
                        }
                    }
                    return Ok(());
                }
                ServiceReadiness::Waiting(detail) => {
                    confirmed_running_without_health = false;
                    if started.elapsed() >= timeout {
                        return Err(anyhow!(
                            "service '{}' ({}) did not become ready within {}s: {}",
                            container_name,
                            service.image,
                            timeout.as_secs(),
                            detail
                        ));
                    }
                    thread::sleep(Duration::from_millis(SERVICE_READY_POLL_MS));
                }
                ServiceReadiness::Failed(detail) => {
                    return Err(anyhow!(
                        "service '{}' ({}) failed readiness check: {}",
                        container_name,
                        service.image,
                        detail
                    ));
                }
            }
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceState {
    running: bool,
    status: Option<String>,
    health: Option<String>,
    exit_code: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServiceReadiness {
    Ready,
    Waiting(String),
    Failed(String),
}

fn service_ready_timeout() -> Duration {
    env::var("OPAL_SERVICE_READY_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(SERVICE_READY_TIMEOUT_DEFAULT_SECS))
}

fn inspect_service_state(engine: EngineKind, container_name: &str) -> Result<ServiceState> {
    let mut command = Command::new(engine_binary(engine));
    command
        .arg("inspect")
        .arg("--format")
        .arg("{{json .State}}")
        .arg(container_name);
    let output = command
        .output()
        .with_context(|| format!("failed to inspect service container '{}'", container_name))?;
    if output.status.success() {
        return parse_service_state(&output.stdout);
    }

    let mut fallback = Command::new(engine_binary(engine));
    fallback.arg("inspect").arg(container_name);
    let output = fallback
        .output()
        .with_context(|| format!("failed to inspect service container '{}'", container_name))?;
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

fn inspect_service_ipv4(engine: EngineKind, container_name: &str) -> Option<String> {
    let mut command = Command::new(engine_binary(engine));
    command.arg("inspect").arg(container_name);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_service_ipv4(&output.stdout).ok().flatten()
}

fn parse_service_ipv4(payload: &[u8]) -> Result<Option<String>> {
    let value: serde_json::Value = serde_json::from_slice(payload)
        .context("failed to parse service inspect output as json")?;
    let service = value
        .as_array()
        .and_then(|items| items.first())
        .or_else(|| value.as_object().map(|_| &value))
        .ok_or_else(|| anyhow!("service inspect output was not an object or array"))?;

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

fn probe_service_ports(
    engine: EngineKind,
    network: &str,
    host: &str,
    ports: &[ServicePort],
) -> Result<bool> {
    if ports.is_empty() {
        return Ok(true);
    }

    let checks = ports
        .iter()
        .filter(|port| port.proto == "tcp")
        .map(|port| format!("nc -z {} {}", shell_escape(host), port.port))
        .collect::<Vec<_>>();
    if checks.is_empty() {
        return Ok(true);
    }

    let mut command = Command::new(engine_binary(engine));
    command.arg("run").arg("--rm").arg("--network").arg(network);
    if matches!(engine, EngineKind::ContainerCli) {
        command.arg("--arch").arg("x86_64");
    }
    let script = format!("{}", checks.join(" && "));
    let status = command
        .arg("docker.io/library/alpine:3.19")
        .arg("sh")
        .arg("-lc")
        .arg(script)
        .status()
        .with_context(|| "failed to run service connectivity probe")?;
    Ok(status.success())
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn parse_service_state(payload: &[u8]) -> Result<ServiceState> {
    let value: serde_json::Value = serde_json::from_slice(payload)
        .context("failed to parse service inspect output as json")?;
    let service = value
        .as_array()
        .and_then(|items| items.first())
        .or_else(|| value.as_object().map(|_| &value))
        .ok_or_else(|| anyhow!("service inspect output was not an object or array"))?;

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
        .and_then(|v| v.as_bool())
        .or_else(|| state.get("running").and_then(|v| v.as_bool()))
        .unwrap_or_else(|| {
            state
                .get("Status")
                .and_then(|v| v.as_str())
                .or_else(|| state.get("status").and_then(|v| v.as_str()))
                .is_some_and(|status| status.eq_ignore_ascii_case("running"))
        });
    let status = state
        .get("Status")
        .and_then(|v| v.as_str())
        .or_else(|| state.get("status").and_then(|v| v.as_str()))
        .map(|s| s.to_ascii_lowercase());
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
        .map(|s| s.to_ascii_lowercase());
    let exit_code = state
        .get("ExitCode")
        .and_then(|v| v.as_i64())
        .or_else(|| state.get("exitCode").and_then(|v| v.as_i64()));

    Ok(ServiceState {
        running,
        status,
        health,
        exit_code,
    })
}

fn readiness_from_state(state: &ServiceState) -> ServiceReadiness {
    if !state.running {
        if matches!(state.status.as_deref(), Some("exited" | "dead" | "stopped"))
            || state.exit_code.is_some_and(|code| code != 0)
        {
            return ServiceReadiness::Failed(format!(
                "status={}, running=false, exit_code={}",
                state.status.as_deref().unwrap_or("unknown"),
                state
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            ));
        }
        return ServiceReadiness::Waiting(format!(
            "status={}, running=false",
            state.status.as_deref().unwrap_or("unknown")
        ));
    }

    match state.health.as_deref() {
        Some("healthy") => ServiceReadiness::Ready,
        Some("unhealthy") => ServiceReadiness::Failed("health=unhealthy".to_string()),
        Some(status) => ServiceReadiness::Waiting(format!("health={status}")),
        None => ServiceReadiness::Ready,
    }
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

fn validate_service_alias(alias: &str) -> Result<String> {
    let normalized = alias.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        bail!("service alias must not be empty");
    }
    if normalized.starts_with('-') || normalized.ends_with('-') {
        bail!("service alias '{}' must not start or end with '-'", alias);
    }
    if !normalized
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        bail!(
            "service alias '{}' contains unsupported characters; use lowercase letters, digits, or '-'",
            alias
        );
    }
    Ok(normalized)
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
    // TODO; jesus on a cracker, what the fuck, for in for in for in for in for.........
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
    let lookup: HashMap<String, String> = base.iter().cloned().collect();
    let mut env = base.to_vec();
    for (_, value) in &mut env {
        *value = crate::env::expand_value(value, &lookup);
    }
    let mut map: HashMap<String, String> = env.into_iter().collect();
    for (key, value) in overrides {
        map.insert(key.clone(), value.clone());
    }
    map.into_iter().collect()
}

fn default_service_aliases(image: &str) -> Vec<String> {
    let without_tag = image.split(':').next().unwrap_or(image);
    let primary = without_tag.replace('/', "__");
    let secondary = without_tag.replace('/', "-");
    let mut aliases = Vec::new();
    if !primary.is_empty() {
        aliases.push(primary);
    }
    if !secondary.is_empty() && !aliases.iter().any(|existing| existing == &secondary) {
        aliases.push(secondary);
    }
    if aliases.is_empty() {
        aliases.push("service".to_string());
    }
    aliases
}

#[cfg(test)]
mod tests {
    use super::{
        ServiceReadiness, ServiceRuntime, ServiceState, parse_service_ipv4, parse_service_state,
        readiness_from_state, should_retry_container_network_error,
    };
    use crate::EngineKind;
    use crate::model::ServiceSpec;
    use std::collections::HashMap;

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

    #[test]
    fn parse_service_state_reads_running_and_health_status() {
        let payload = br#"[{"State":{"Running":true,"Status":"running","ExitCode":0,"Health":{"Status":"starting"}}}]"#;
        let state = parse_service_state(payload).expect("parse service state");

        assert!(state.running);
        assert_eq!(state.status.as_deref(), Some("running"));
        assert_eq!(state.health.as_deref(), Some("starting"));
        assert_eq!(state.exit_code, Some(0));
    }

    #[test]
    fn parse_service_state_accepts_direct_state_object() {
        let payload = br#"{"Running":false,"Status":"exited","ExitCode":1}"#;
        let state = parse_service_state(payload).expect("parse service state");

        assert!(!state.running);
        assert_eq!(state.status.as_deref(), Some("exited"));
        assert_eq!(state.exit_code, Some(1));
    }

    #[test]
    fn parse_service_state_accepts_container_cli_shape() {
        let payload = br#"[{"status":"exited","exitCode":1}]"#;
        let state = parse_service_state(payload).expect("parse service state");

        assert!(!state.running);
        assert_eq!(state.status.as_deref(), Some("exited"));
        assert_eq!(state.exit_code, Some(1));
    }

    #[test]
    fn readiness_from_state_is_ready_without_healthcheck() {
        let state = ServiceState {
            running: true,
            status: Some("running".to_string()),
            health: None,
            exit_code: Some(0),
        };

        assert!(matches!(
            readiness_from_state(&state),
            ServiceReadiness::Ready
        ));
    }

    #[test]
    fn readiness_from_state_waits_while_healthcheck_is_starting() {
        let state = ServiceState {
            running: true,
            status: Some("running".to_string()),
            health: Some("starting".to_string()),
            exit_code: Some(0),
        };

        match readiness_from_state(&state) {
            ServiceReadiness::Waiting(detail) => assert!(detail.contains("starting")),
            other => panic!("expected waiting readiness, got {other:?}"),
        }
    }

    #[test]
    fn readiness_from_state_fails_when_service_exits() {
        let state = ServiceState {
            running: false,
            status: Some("exited".to_string()),
            health: None,
            exit_code: Some(1),
        };

        match readiness_from_state(&state) {
            ServiceReadiness::Failed(detail) => assert!(detail.contains("exit_code=1")),
            other => panic!("expected failed readiness, got {other:?}"),
        }
    }

    #[test]
    fn readiness_from_state_fails_when_healthcheck_unhealthy() {
        let state = ServiceState {
            running: true,
            status: Some("running".to_string()),
            health: Some("unhealthy".to_string()),
            exit_code: Some(0),
        };

        match readiness_from_state(&state) {
            ServiceReadiness::Failed(detail) => assert!(detail.contains("unhealthy")),
            other => panic!("expected failed readiness, got {other:?}"),
        }
    }

    #[test]
    fn aliases_for_service_preserves_multiple_unique_aliases() {
        let mut runtime = ServiceRuntime {
            engine: EngineKind::Docker,
            network: "net".into(),
            containers: Vec::new(),
            link_env: Vec::new(),
            claimed_aliases: Default::default(),
            host_aliases: Vec::new(),
        };
        let service = ServiceSpec {
            image: "redis:7".into(),
            aliases: vec!["cache".into(), "redis".into()],
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };

        let aliases = runtime
            .aliases_for_service(0, &service)
            .expect("aliases resolve");

        assert_eq!(aliases, vec!["cache", "redis"]);
    }

    #[test]
    fn aliases_for_service_uses_gitlab_style_default_aliases() {
        let mut runtime = ServiceRuntime {
            engine: EngineKind::Docker,
            network: "net".into(),
            containers: Vec::new(),
            link_env: Vec::new(),
            claimed_aliases: Default::default(),
            host_aliases: Vec::new(),
        };
        let service = ServiceSpec {
            image: "tutum/wordpress:latest".into(),
            aliases: Vec::new(),
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };

        let aliases = runtime
            .aliases_for_service(0, &service)
            .expect("aliases resolve");

        assert_eq!(aliases, vec!["tutum__wordpress", "tutum-wordpress"]);
    }

    #[test]
    fn aliases_for_service_falls_back_when_aliases_conflict() {
        let mut runtime = ServiceRuntime {
            engine: EngineKind::Docker,
            network: "net".into(),
            containers: Vec::new(),
            link_env: Vec::new(),
            claimed_aliases: Default::default(),
            host_aliases: Vec::new(),
        };
        let first = ServiceSpec {
            image: "redis:7".into(),
            aliases: vec!["cache".into()],
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };
        let second = ServiceSpec {
            image: "postgres:16".into(),
            aliases: vec!["cache".into()],
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };

        assert_eq!(
            runtime.aliases_for_service(0, &first).unwrap(),
            vec!["cache"]
        );
        assert_eq!(
            runtime.aliases_for_service(1, &second).unwrap(),
            vec!["svc-1"]
        );
    }

    #[test]
    fn aliases_for_service_falls_back_after_default_aliases_conflict() {
        let mut runtime = ServiceRuntime {
            engine: EngineKind::Docker,
            network: "net".into(),
            containers: Vec::new(),
            link_env: Vec::new(),
            claimed_aliases: Default::default(),
            host_aliases: Vec::new(),
        };
        let first = ServiceSpec {
            image: "tutum/wordpress:latest".into(),
            aliases: Vec::new(),
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };
        let second = ServiceSpec {
            image: "tutum/wordpress:latest".into(),
            aliases: Vec::new(),
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };

        assert_eq!(
            runtime.aliases_for_service(0, &first).unwrap(),
            vec!["tutum__wordpress", "tutum-wordpress"]
        );
        assert_eq!(
            runtime.aliases_for_service(1, &second).unwrap(),
            vec!["svc-1"]
        );
    }

    #[test]
    fn aliases_for_service_rejects_invalid_aliases() {
        let mut runtime = ServiceRuntime {
            engine: EngineKind::Docker,
            network: "net".into(),
            containers: Vec::new(),
            link_env: Vec::new(),
            claimed_aliases: Default::default(),
            host_aliases: Vec::new(),
        };
        let service = ServiceSpec {
            image: "redis:7".into(),
            aliases: vec!["bad_alias".into()],
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };

        let err = runtime
            .aliases_for_service(0, &service)
            .expect_err("alias must error");
        assert!(err.to_string().contains("unsupported characters"));
    }

    #[test]
    fn parse_service_ipv4_accepts_container_cli_shape() {
        let payload = br#"[{"networks":[{"ipv4Address":"192.168.64.57/24"}]}]"#;
        assert_eq!(
            parse_service_ipv4(payload).unwrap(),
            Some("192.168.64.57".into())
        );
    }

    #[test]
    fn parse_service_ipv4_accepts_docker_shape() {
        let payload = br#"[{"NetworkSettings":{"Networks":{"opal":{"IPAddress":"172.18.0.2"}}}}]"#;
        assert_eq!(
            parse_service_ipv4(payload).unwrap(),
            Some("172.18.0.2".into())
        );
    }

    #[test]
    fn merged_env_does_not_expand_service_only_variables() {
        let merged = super::merged_env(
            &[("BASE".into(), "hello".into())],
            &HashMap::from([
                ("BASE".into(), "override".into()),
                ("SERVICE_ONLY".into(), "$BASE-world".into()),
            ]),
        );
        let merged_map: HashMap<_, _> = merged.into_iter().collect();

        assert_eq!(merged_map.get("BASE").map(String::as_str), Some("override"));
        assert_eq!(
            merged_map.get("SERVICE_ONLY").map(String::as_str),
            Some("$BASE-world")
        );
    }
}
