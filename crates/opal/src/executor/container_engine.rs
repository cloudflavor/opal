use crate::engine::EngineCommandContext;
use std::process::{Command, Stdio};

pub(super) fn build_container_engine_command(
    binary: &str,
    ctx: &EngineCommandContext<'_>,
) -> Command {
    let mut command = base_command(binary, ctx);
    append_workspace_volume(&mut command, ctx);
    append_mounts(&mut command, ctx);
    append_network(&mut command, ctx);
    append_platform(&mut command, ctx);
    append_image_options(&mut command, ctx);
    append_privileges(&mut command, ctx);
    append_env(&mut command, ctx);
    append_container_script(&mut command, ctx);
    command
}

fn base_command(binary: &str, ctx: &EngineCommandContext<'_>) -> Command {
    let mut command = Command::new(binary);
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("run")
        .arg("--name")
        .arg(ctx.container_name)
        .arg("--workdir")
        .arg(ctx.container_root);
    command
}

fn workspace_mount(ctx: &EngineCommandContext<'_>) -> String {
    format!("{}:{}", ctx.workdir.display(), ctx.container_root.display())
}

fn append_workspace_volume(command: &mut Command, ctx: &EngineCommandContext<'_>) {
    command.arg("--volume").arg(workspace_mount(ctx));
}

fn append_mounts(command: &mut Command, ctx: &EngineCommandContext<'_>) {
    for mount in ctx.mounts {
        command.arg("--volume").arg(mount.to_arg());
    }
}

fn append_network(command: &mut Command, ctx: &EngineCommandContext<'_>) {
    if let Some(network) = ctx.network {
        command.arg("--network").arg(network);
    }
}

fn append_platform(command: &mut Command, ctx: &EngineCommandContext<'_>) {
    if let Some(platform) = ctx.image_platform {
        command.arg("--platform").arg(platform);
    }
}

fn append_image_options(command: &mut Command, ctx: &EngineCommandContext<'_>) {
    if let Some(user) = ctx.image_user {
        command.arg("--user").arg(user);
    }
    if !ctx.image_entrypoint.is_empty() {
        command
            .arg("--entrypoint")
            .arg(ctx.image_entrypoint.join(" "));
    }
}

fn append_privileges(command: &mut Command, ctx: &EngineCommandContext<'_>) {
    if ctx.privileged {
        command.arg("--privileged");
    }
    for capability in ctx.cap_add {
        command.arg("--cap-add").arg(capability);
    }
    for capability in ctx.cap_drop {
        command.arg("--cap-drop").arg(capability);
    }
}

fn append_env(command: &mut Command, ctx: &EngineCommandContext<'_>) {
    for (key, value) in ctx.env_vars {
        command.arg("--env").arg(format!("{key}={value}"));
    }
}

fn append_container_script(command: &mut Command, ctx: &EngineCommandContext<'_>) {
    command.arg(ctx.image).arg("sh").arg(ctx.container_script);
}

#[cfg(test)]
mod tests {
    use super::build_container_engine_command;
    use crate::engine::EngineCommandContext;
    use crate::pipeline::VolumeMount;
    use std::path::Path;

    fn base_context<'a>() -> EngineCommandContext<'a> {
        EngineCommandContext {
            workdir: Path::new("/workspace"),
            container_root: Path::new("/builds/workspace"),
            container_script: Path::new("/opal/script.sh"),
            container_name: "opal-job",
            image: "alpine:3.19",
            image_platform: None,
            image_user: None,
            image_entrypoint: &[],
            mounts: &[],
            env_vars: &[],
            network: None,
            preserve_runtime_objects: false,
            arch: None,
            privileged: false,
            cap_add: &[],
            cap_drop: &[],
            cpus: None,
            memory: None,
            dns: None,
        }
    }

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect()
    }

    use std::process::Command;

    #[test]
    fn build_command_mounts_workspace_before_nested_artifacts() {
        let mounts = [VolumeMount {
            host: "/tmp/artifacts".into(),
            container: "/builds/workspace/tests-temp/shared".into(),
            read_only: true,
        }];
        let mut ctx = base_context();
        ctx.mounts = &mounts;

        let args = command_args(&build_container_engine_command("docker", &ctx));
        let workspace_mount = "/workspace:/builds/workspace";
        let artifact_mount = "/tmp/artifacts:/builds/workspace/tests-temp/shared:ro";
        let workspace_idx = args
            .iter()
            .position(|arg| arg == workspace_mount)
            .expect("workspace mount present");
        let artifact_idx = args
            .iter()
            .position(|arg| arg == artifact_mount)
            .expect("artifact mount present");

        assert!(workspace_idx < artifact_idx);
    }

    #[test]
    fn build_command_includes_platform_when_requested() {
        let mut ctx = base_context();
        ctx.image_platform = Some("linux/arm64/v8");

        let args = command_args(&build_container_engine_command("docker", &ctx));

        assert!(
            args.windows(2)
                .any(|pair| pair == ["--platform", "linux/arm64/v8"])
        );
    }

    #[test]
    fn build_command_includes_privileged_and_capabilities() {
        let cap_add = vec!["NET_ADMIN".to_string()];
        let cap_drop = vec!["MKNOD".to_string()];
        let mut ctx = base_context();
        ctx.privileged = true;
        ctx.cap_add = &cap_add;
        ctx.cap_drop = &cap_drop;

        let args = command_args(&build_container_engine_command("docker", &ctx));

        assert!(args.iter().any(|arg| arg == "--privileged"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--cap-add", "NET_ADMIN"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--cap-drop", "MKNOD"]));
    }

    #[test]
    fn build_command_includes_network_user_entrypoint_and_env_vars() {
        let entrypoint = vec!["/bin/sh".to_string(), "-lc".to_string()];
        let env_vars = vec![("FOO".to_string(), "bar".to_string())];
        let mut ctx = base_context();
        ctx.network = Some("opal-network");
        ctx.image_user = Some("1000:1000");
        ctx.image_entrypoint = &entrypoint;
        ctx.env_vars = &env_vars;

        let args = command_args(&build_container_engine_command("docker", &ctx));

        assert!(
            args.windows(2)
                .any(|pair| pair == ["--network", "opal-network"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--user", "1000:1000"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--entrypoint", "/bin/sh -lc"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--env", "FOO=bar"]));
    }
}
