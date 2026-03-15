use crate::EngineKind;
use crate::env::expand_env_list;
use crate::gitlab::ServiceConfig;
use crate::naming::job_name_slug;
use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::process::Command;

pub struct ServiceRuntime {
    engine: EngineKind,
    network: String,
    containers: Vec<String>,
}

impl ServiceRuntime {
    pub fn start(
        engine: EngineKind,
        run_id: &str,
        job_name: &str,
        services: &[ServiceConfig],
        base_env: &[(String, String)],
        shared_env: &HashMap<String, String>,
    ) -> Result<Option<Self>> {
        if services.is_empty() {
            return Ok(None);
        }
        service_supported(engine)?;
        let network = format!(
            "opal-net-{}-{}",
            run_id.replace(|c: char| !c.is_ascii_alphanumeric(), ""),
            job_name_slug(job_name)
        );
        let mut network_cmd = Command::new(engine_binary(engine));
        network_cmd.arg("network").arg("create").arg(&network);
        run_command(network_cmd)
            .with_context(|| format!("failed to create network {}", network))?;

        let mut runtime = ServiceRuntime {
            engine,
            network: network.clone(),
            containers: Vec::new(),
        };

        for (idx, service) in services.iter().enumerate() {
            let container_name = format!(
                "opal-svc-{}-{}-{:02}",
                run_id.replace(|c: char| !c.is_ascii_alphanumeric(), ""),
                job_name_slug(job_name),
                idx
            );
            if let Err(err) = runtime.start_service(&container_name, service, base_env, shared_env)
            {
                runtime.cleanup();
                return Err(err);
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
        let _ = Command::new(engine_binary(self.engine))
            .arg("network")
            .arg("rm")
            .arg(&self.network)
            .status();
    }

    fn start_service(
        &mut self,
        container_name: &str,
        service: &ServiceConfig,
        base_env: &[(String, String)],
        shared_env: &HashMap<String, String>,
    ) -> Result<()> {
        let alias = service
            .alias
            .clone()
            .unwrap_or_else(|| default_service_alias(&service.image));
        let mut command = service_command(self.engine);
        command
            .arg("-d")
            .arg("--rm")
            .arg("--name")
            .arg(container_name)
            .arg("--network")
            .arg(&self.network)
            .arg("--network-alias")
            .arg(&alias);

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

        for arg in &service.command {
            command.arg(arg);
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
        command.arg("--dns").arg("1.1.1.1");
    }
    command
}

fn run_command(mut cmd: Command) -> Result<()> {
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "command {:?} exited with status {:?}",
            cmd,
            status.code()
        ))
    }
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
