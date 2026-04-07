use super::command::engine_binary;
use super::inspect::{ServiceInspector, ServicePort, ServiceState};
use crate::EngineKind;
use crate::executor::container_arch::default_container_cli_arch;
use crate::model::ServiceSpec;
use anyhow::{Result, anyhow};
use std::env;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use tracing::warn;

const SERVICE_READY_TIMEOUT_DEFAULT_SECS: u64 = 30;
const SERVICE_READY_POLL_MS: u64 = 250;

pub(super) struct ServiceReadinessProbe {
    engine: EngineKind,
    network: String,
}

impl ServiceReadinessProbe {
    pub(super) fn new(engine: EngineKind, network: String) -> Self {
        Self { engine, network }
    }

    pub(super) fn wait_for(
        &self,
        inspector: &ServiceInspector,
        container_name: &str,
        service: &ServiceSpec,
        ports: &[ServicePort],
    ) -> Result<()> {
        let timeout = service_ready_timeout();
        let started = Instant::now();
        let mut confirmed_running_without_health = false;

        loop {
            let state = match inspector.state(container_name) {
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
                            let Some(ip) = inspector.ipv4(container_name) else {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServiceReadiness {
    Ready,
    Waiting(String),
    Failed(String),
}

pub(super) fn readiness_from_state(state: &ServiceState) -> ServiceReadiness {
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
    if matches!(engine, EngineKind::ContainerCli)
        && let Some(arch) = default_container_cli_arch(None)
    {
        command.arg("--arch").arg(arch);
    }
    let status = command
        .arg("docker.io/library/alpine:3.19")
        .arg("sh")
        .arg("-lc")
        .arg(checks.join(" && "))
        .status()?;
    Ok(status.success())
}

fn service_ready_timeout() -> Duration {
    env::var("OPAL_SERVICE_READY_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(SERVICE_READY_TIMEOUT_DEFAULT_SECS))
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
