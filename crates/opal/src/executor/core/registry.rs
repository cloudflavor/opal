use super::ExecutorCore;
use crate::EngineKind;
use crate::config::ResolvedRegistryAuth;
use anyhow::{Context, Result, anyhow};
use std::io::Write;
use std::process::{Command, Stdio};

pub(super) fn ensure_registry_logins(exec: &ExecutorCore) -> Result<()> {
    let auths = exec.config.settings.registry_auth_for(exec.config.engine)?;
    for auth in &auths {
        match exec.config.engine {
            EngineKind::ContainerCli => container_registry_login(auth)?,
            EngineKind::Docker | EngineKind::Orbstack => standard_registry_login("docker", auth)?,
            EngineKind::Podman => standard_registry_login("podman", auth)?,
            EngineKind::Nerdctl => standard_registry_login("nerdctl", auth)?,
        }
    }
    Ok(())
}

fn container_registry_login(auth: &ResolvedRegistryAuth) -> Result<()> {
    let mut child = container_registry_login_command(auth)
        .spawn()
        .with_context(|| format!("failed to run container registry login for {}", auth.server))?;
    child
        .stdin
        .as_mut()
        .context("missing stdin for container registry login")?
        .write_all(auth.password.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow!(
            "container registry login for {} failed with status {:?}",
            auth.server,
            status.code()
        ));
    }
    Ok(())
}

fn standard_registry_login(binary: &str, auth: &ResolvedRegistryAuth) -> Result<()> {
    let mut child = standard_registry_login_command(binary, auth)
        .spawn()
        .with_context(|| format!("failed to run {} login for {}", binary, auth.server))?;
    child
        .stdin
        .as_mut()
        .context("missing stdin for registry login")?
        .write_all(auth.password.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow!(
            "{} login for {} failed with status {:?}",
            binary,
            auth.server,
            status.code()
        ));
    }
    Ok(())
}

fn container_registry_login_command(auth: &ResolvedRegistryAuth) -> Command {
    let mut command = Command::new("container");
    command.arg("registry").arg("login");
    if let Some(scheme) = auth.scheme.as_deref() {
        command.arg("--scheme").arg(scheme);
    }
    command
        .arg("--username")
        .arg(&auth.username)
        .arg("--password-stdin")
        .arg(&auth.server)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

fn standard_registry_login_command(binary: &str, auth: &ResolvedRegistryAuth) -> Command {
    let mut command = Command::new(binary);
    command
        .arg("login")
        .arg("--username")
        .arg(&auth.username)
        .arg("--password-stdin")
        .arg(&auth.server)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

#[cfg(test)]
mod tests {
    use super::{container_registry_login_command, standard_registry_login_command};
    use crate::config::ResolvedRegistryAuth;

    #[test]
    fn container_registry_login_command_includes_scheme_when_present() {
        let auth = ResolvedRegistryAuth {
            server: "registry.example.com".into(),
            username: "user".into(),
            password: "secret".into(),
            scheme: Some("https".into()),
        };

        let command = container_registry_login_command(&auth);
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert_eq!(command.get_program().to_string_lossy(), "container");
        assert!(args.windows(2).any(|pair| pair == ["registry", "login"]));
        assert!(args.windows(2).any(|pair| pair == ["--scheme", "https"]));
        assert!(args.windows(2).any(|pair| pair == ["--username", "user"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--password-stdin", "registry.example.com"])
        );
    }

    #[test]
    fn standard_registry_login_command_targets_requested_binary() {
        let auth = ResolvedRegistryAuth {
            server: "registry.example.com".into(),
            username: "user".into(),
            password: "secret".into(),
            scheme: None,
        };

        let command = standard_registry_login_command("podman", &auth);
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert_eq!(command.get_program().to_string_lossy(), "podman");
        assert!(args.windows(2).any(|pair| pair == ["login", "--username"]));
        assert!(args.windows(2).any(|pair| pair == ["--username", "user"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--password-stdin", "registry.example.com"])
        );
    }
}
