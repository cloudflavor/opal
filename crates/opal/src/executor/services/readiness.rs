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
    preserve_runtime_objects: bool,
}

impl ServiceReadinessProbe {
    pub(super) fn new(engine: EngineKind, network: String, preserve_runtime_objects: bool) -> Self {
        Self {
            engine,
            network,
            preserve_runtime_objects,
        }
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
            let ready_check = ReadyCheck {
                inspector,
                container_name,
                service,
                ports,
                started,
                timeout,
            };

            match readiness_from_state(&state) {
                ServiceReadiness::Ready => match self
                    .await_ready_service(&ready_check, confirmed_running_without_health)?
                {
                    ReadinessPoll::Ready => return Ok(()),
                    ReadinessPoll::Retry {
                        confirmed_running_without_health: confirmed,
                    } => {
                        confirmed_running_without_health = confirmed;
                        continue;
                    }
                },
                ServiceReadiness::Waiting(detail) => {
                    confirmed_running_without_health = false;
                    wait_for_retry_or_timeout(
                        started,
                        timeout,
                        format!(
                            "service '{}' ({}) did not become ready within {}s: {}",
                            container_name,
                            service.image,
                            timeout.as_secs(),
                            detail
                        ),
                    )?;
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

    fn await_ready_service(
        &self,
        ready_check: &ReadyCheck<'_>,
        confirmed_running_without_health: bool,
    ) -> Result<ReadinessPoll> {
        if ready_check.ports.is_empty() {
            return await_running_confirmation(
                ready_check.started,
                ready_check.timeout,
                ready_check.container_name,
                &ready_check.service.image,
                confirmed_running_without_health,
            );
        }

        let Some(ip) = ready_check.inspector.ipv4(ready_check.container_name) else {
            wait_for_retry_or_timeout(
                ready_check.started,
                ready_check.timeout,
                format!(
                    "service '{}' ({}) did not expose a reachable IP within {}s",
                    ready_check.container_name,
                    ready_check.service.image,
                    ready_check.timeout.as_secs()
                ),
            )?;
            return Ok(ReadinessPoll::retry(false));
        };

        if probe_service_ports(
            self.engine,
            &self.network,
            &ip,
            ready_check.ports,
            self.preserve_runtime_objects,
        )? {
            return Ok(ReadinessPoll::Ready);
        }

        wait_for_retry_or_timeout(
            ready_check.started,
            ready_check.timeout,
            format!(
                "service '{}' ({}) did not accept connections on exposed ports within {}s",
                ready_check.container_name,
                ready_check.service.image,
                ready_check.timeout.as_secs()
            ),
        )?;
        Ok(ReadinessPoll::retry(false))
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
    preserve_runtime_objects: bool,
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

    let mut command = service_probe_command(engine, network, preserve_runtime_objects);
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

pub(super) fn service_probe_command(
    engine: EngineKind,
    network: &str,
    preserve_runtime_objects: bool,
) -> Command {
    let mut command = Command::new(engine_binary(engine));
    command.arg("run");
    if !preserve_runtime_objects {
        command.arg("--rm");
    }
    command.arg("--network").arg(network);
    command
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

enum ReadinessPoll {
    Ready,
    Retry {
        confirmed_running_without_health: bool,
    },
}

impl ReadinessPoll {
    fn retry(confirmed_running_without_health: bool) -> Self {
        Self::Retry {
            confirmed_running_without_health,
        }
    }
}

struct ReadyCheck<'a> {
    inspector: &'a ServiceInspector,
    container_name: &'a str,
    service: &'a ServiceSpec,
    ports: &'a [ServicePort],
    started: Instant,
    timeout: Duration,
}

fn wait_for_retry_or_timeout(
    started: Instant,
    timeout: Duration,
    timeout_message: String,
) -> Result<()> {
    if started.elapsed() >= timeout {
        return Err(anyhow!(timeout_message));
    }
    thread::sleep(Duration::from_millis(SERVICE_READY_POLL_MS));
    Ok(())
}

fn await_running_confirmation(
    started: Instant,
    timeout: Duration,
    container_name: &str,
    image: &str,
    confirmed_running_without_health: bool,
) -> Result<ReadinessPoll> {
    if confirmed_running_without_health {
        return Ok(ReadinessPoll::Ready);
    }

    wait_for_retry_or_timeout(
        started,
        timeout,
        format!(
            "service '{}' ({}) did not remain running long enough to confirm readiness within {}s",
            container_name,
            image,
            timeout.as_secs()
        ),
    )?;
    Ok(ReadinessPoll::retry(true))
}
