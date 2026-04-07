mod alias;
mod command;
mod inspect;
mod network;
mod readiness;

use self::alias::ServiceAliasRegistry;
use self::command::{
    command_timeout, engine_binary, merged_env, run_command_with_timeout, service_command,
};
use self::inspect::{ServiceInspector, ServicePort};
use self::network::ServiceNetworkManager;
use self::readiness::ServiceReadinessProbe;
use crate::EngineKind;
use crate::model::ServiceSpec;
use crate::naming::job_name_slug;
use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fmt::Write as FmtWrite;
use std::process::Command;
use tracing::warn;

const MAX_NAME_LEN: usize = 63;

pub struct ServiceRuntime {
    lifecycle: ServiceLifecycle,
    link_env: Vec<(String, String)>,
    aliases: ServiceAliasRegistry,
    host_aliases: Vec<(String, String)>,
}

impl ServiceRuntime {
    pub fn start(
        engine: EngineKind,
        run_id: &str,
        job_name: &str,
        services: &[ServiceSpec],
        base_env: &[(String, String)],
        _shared_env: &HashMap<String, String>,
    ) -> Result<Option<Self>> {
        if services.is_empty() {
            return Ok(None);
        }
        service_supported(engine)?;

        let network = service_network_name(run_id, job_name);
        let network_manager = ServiceNetworkManager::new(engine);
        network_manager
            .create(&network)
            .with_context(|| format!("failed to create network {network}"))?;

        let inspector = ServiceInspector::new(engine);
        let readiness = ServiceReadinessProbe::new(engine, network.clone());
        let mut runtime = ServiceRuntime {
            lifecycle: ServiceLifecycle::new(engine, network),
            link_env: Vec::new(),
            aliases: ServiceAliasRegistry::new(),
            host_aliases: Vec::new(),
        };

        for (idx, service) in services.iter().enumerate() {
            let aliases = runtime.aliases_for_service(idx, service)?;
            let container_name = service_container_name(run_id, job_name, idx);
            let ports = discover_service_ports(engine, &inspector, service);

            if let Err(err) =
                runtime
                    .lifecycle
                    .start_service(&container_name, service, &aliases, base_env)
            {
                runtime.cleanup();
                return Err(err);
            }
            if let Err(err) = readiness.wait_for(&inspector, &container_name, service, &ports) {
                runtime.cleanup();
                return Err(err);
            }
            if let Some(ip) = inspector.ipv4(&container_name) {
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
        self.lifecycle.network_name()
    }

    pub fn container_names(&self) -> &[String] {
        self.lifecycle.container_names()
    }

    pub fn cleanup(&mut self) {
        self.lifecycle.cleanup();
    }

    pub fn link_env(&self) -> &[(String, String)] {
        &self.link_env
    }

    pub fn host_aliases(&self) -> &[(String, String)] {
        &self.host_aliases
    }

    fn aliases_for_service(&mut self, idx: usize, service: &ServiceSpec) -> Result<Vec<String>> {
        self.aliases.aliases_for_service(idx, service)
    }
}

struct ServiceLifecycle {
    engine: EngineKind,
    network: String,
    containers: Vec<String>,
}

impl ServiceLifecycle {
    fn new(engine: EngineKind, network: String) -> Self {
        Self {
            engine,
            network,
            containers: Vec::new(),
        }
    }

    fn network_name(&self) -> &str {
        &self.network
    }

    fn container_names(&self) -> &[String] {
        &self.containers
    }

    fn start_service(
        &mut self,
        container_name: &str,
        service: &ServiceSpec,
        aliases: &[String],
        base_env: &[(String, String)],
    ) -> Result<()> {
        let mut command = service_command(self.engine, service);
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

        if let Some(user) = &service.docker_user {
            command.arg("--user").arg(user);
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
            .map(|value| value == "1")
            .unwrap_or(false)
        {
            let program = command.get_program().to_string_lossy();
            let args: Vec<String> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect();
            eprintln!("[opal] service command: {} {}", program, args.join(" "));
        }

        run_command_with_timeout(command, command_timeout(self.engine)).with_context(|| {
            format!(
                "failed to start service '{}' ({})",
                container_name, service.image
            )
        })?;
        self.containers.push(container_name.to_string());
        Ok(())
    }

    fn cleanup(&mut self) {
        for name in self.containers.drain(..).rev() {
            let _ = Command::new(engine_binary(self.engine))
                .arg("rm")
                .arg("-f")
                .arg(&name)
                .status();
        }
        let _ = ServiceNetworkManager::new(self.engine).remove(&self.network);
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
            "services are only supported when using docker, podman, nerdctl, orbstack, or container"
        ))
    }
}

fn service_network_name(run_id: &str, job_name: &str) -> String {
    let clean_run_id = sanitize_identifier(run_id);
    clamp_name(&format!(
        "opal-net-{}-{}",
        clean_run_id,
        job_name_slug(job_name)
    ))
}

fn service_container_name(run_id: &str, job_name: &str, idx: usize) -> String {
    let clean_run_id = sanitize_identifier(run_id);
    clamp_name(&format!(
        "opal-svc-{}-{}-{:02}",
        clean_run_id,
        job_name_slug(job_name),
        idx
    ))
}

fn discover_service_ports(
    engine: EngineKind,
    inspector: &ServiceInspector,
    service: &ServiceSpec,
) -> Vec<ServicePort> {
    if !matches!(engine, EngineKind::ContainerCli) {
        return Vec::new();
    }

    match inspector.discover_ports(&service.image) {
        Ok(ports) => ports,
        Err(err) => {
            warn!(
                image = %service.image,
                "failed to detect exposed ports for service: {err}"
            );
            Vec::new()
        }
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

fn build_service_env(alias: &str, host: &str, ports: &[ServicePort]) -> Vec<(String, String)> {
    if ports.is_empty() {
        return Vec::new();
    }

    let mut envs = Vec::new();
    let alias_key: String = alias
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let primary = &ports[0];
    envs.push((
        format!("{alias_key}_PORT"),
        format!("{}://{}:{}", primary.proto, host, primary.port),
    ));
    for port in ports {
        let proto_upper = port.proto.to_ascii_uppercase();
        let proto_lower = port.proto.to_ascii_lowercase();
        let base = format!("{alias_key}_PORT_{}_{}", port.port, proto_upper);
        envs.push((
            base.clone(),
            format!("{proto_lower}://{host}:{}", port.port),
        ));
        envs.push((format!("{base}_ADDR"), host.to_string()));
        envs.push((format!("{base}_PORT"), port.port.to_string()));
        envs.push((format!("{base}_PROTO"), proto_lower));
    }
    envs
}

#[cfg(test)]
mod tests {
    use super::alias::ServiceAliasRegistry;
    use super::command::{run_command_with_timeout, service_command};
    use super::inspect::{ServiceState, parse_service_ipv4, parse_service_state};
    use super::network::should_retry_container_network_error;
    use super::readiness::{ServiceReadiness, readiness_from_state};
    use super::{ServiceLifecycle, ServiceRuntime};
    use crate::EngineKind;
    use crate::model::ServiceSpec;
    use std::collections::HashMap;
    use std::process::Command;
    use std::time::Duration;

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
        let mut runtime = test_runtime();
        let service = ServiceSpec {
            image: "redis:7".into(),
            aliases: vec!["cache".into(), "redis".into()],
            docker_platform: None,
            docker_user: None,
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
        let mut runtime = test_runtime();
        let service = ServiceSpec {
            image: "tutum/wordpress:latest".into(),
            aliases: Vec::new(),
            docker_platform: None,
            docker_user: None,
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
        let mut runtime = test_runtime();
        let first = ServiceSpec {
            image: "redis:7".into(),
            aliases: vec!["cache".into()],
            docker_platform: None,
            docker_user: None,
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };
        let second = ServiceSpec {
            image: "postgres:16".into(),
            aliases: vec!["cache".into()],
            docker_platform: None,
            docker_user: None,
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
        let mut runtime = test_runtime();
        let first = ServiceSpec {
            image: "tutum/wordpress:latest".into(),
            aliases: Vec::new(),
            docker_platform: None,
            docker_user: None,
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };
        let second = ServiceSpec {
            image: "tutum/wordpress:latest".into(),
            aliases: Vec::new(),
            docker_platform: None,
            docker_user: None,
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
        let mut runtime = test_runtime();
        let service = ServiceSpec {
            image: "redis:7".into(),
            aliases: vec!["bad_alias".into()],
            docker_platform: None,
            docker_user: None,
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
    fn service_command_for_docker_forwards_platform_and_user() {
        let service = ServiceSpec {
            image: "redis:7".into(),
            aliases: vec!["cache".into()],
            docker_platform: Some("linux/arm64/v8".into()),
            docker_user: Some("1000:1000".into()),
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };

        let mut command = service_command(EngineKind::Docker, &service);
        command
            .arg("--user")
            .arg(service.docker_user.as_deref().unwrap());
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert!(
            args.windows(2)
                .any(|pair| pair == ["--platform", "linux/arm64/v8"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--user", "1000:1000"]));
    }

    #[test]
    fn service_command_for_container_cli_translates_platform_to_arch() {
        let service = ServiceSpec {
            image: "redis:7".into(),
            aliases: vec!["cache".into()],
            docker_platform: Some("linux/amd64".into()),
            docker_user: Some("1000:1000".into()),
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        };

        let mut command = service_command(EngineKind::ContainerCli, &service);
        command
            .arg("--user")
            .arg(service.docker_user.as_deref().unwrap());
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert!(args.windows(2).any(|pair| pair == ["--arch", "x86_64"]));
        assert!(args.windows(2).any(|pair| pair == ["--user", "1000:1000"]));
    }

    #[test]
    fn run_command_with_timeout_fails_fast() {
        let mut command = Command::new("sh");
        command.arg("-lc").arg("sleep 1");

        let err = run_command_with_timeout(command, Some(Duration::from_millis(50)))
            .expect_err("command should time out");

        assert!(err.to_string().contains("timed out"));
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
        let merged = super::command::merged_env(
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

    fn test_runtime() -> ServiceRuntime {
        ServiceRuntime {
            lifecycle: ServiceLifecycle::new(EngineKind::Docker, "net".into()),
            link_env: Vec::new(),
            aliases: ServiceAliasRegistry::new(),
            host_aliases: Vec::new(),
        }
    }
}
