use super::container_arch::default_container_cli_arch;
use super::core::{ExecutionOutcome, ExecutorCore};
use crate::engine::EngineCommandContext;
use crate::{EngineKind, ExecutorConfig};
use anyhow::Result;
use std::process::{Command, Stdio};

const DEFAULT_MEMORY_LIMIT: &str = "1638m"; // ~1.6 GB
const DEFAULT_CPU_LIMIT: &str = "4";

#[derive(Debug, Clone)]
pub struct ContainerExecutor {
    core: ExecutorCore,
}

impl ContainerExecutor {
    pub fn new(mut config: ExecutorConfig) -> Result<Self> {
        config.engine = EngineKind::ContainerCli;
        let core = ExecutorCore::new(config)?;
        Ok(Self { core })
    }

    pub async fn run(&self) -> ExecutionOutcome {
        self.core.run().await
    }

    pub fn build_command(ctx: &EngineCommandContext<'_>) -> Command {
        ContainerCommandBuilder::new(ctx)
            .with_workspace_volume()
            .with_volumes()
            .with_network()
            .with_image_options()
            .with_env()
            .build()
    }
}

struct ContainerCommandBuilder<'a> {
    ctx: &'a EngineCommandContext<'a>,
    command: Command,
    workspace_mount: String,
}

impl<'a> ContainerCommandBuilder<'a> {
    fn new(ctx: &'a EngineCommandContext<'a>) -> Self {
        let workspace_mount = format!("{}:{}", ctx.workdir.display(), ctx.container_root.display());
        let cpus = ctx.cpus.unwrap_or(DEFAULT_CPU_LIMIT);
        let memory = ctx.memory.unwrap_or(DEFAULT_MEMORY_LIMIT);
        let mut command = Command::new("container");
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .arg("run")
            .arg("--name")
            .arg(ctx.container_name)
            .arg("--workdir")
            .arg(ctx.container_root)
            .arg("--cpus")
            .arg(cpus)
            .arg("--memory")
            .arg(memory);
        if !ctx.preserve_runtime_objects {
            command.arg("--rm");
        }
        if let Some(arch) = ctx
            .arch
            .map(str::to_string)
            .or_else(|| default_container_cli_arch(ctx.image_platform))
        {
            command.arg("--arch").arg(arch);
        }
        // TODO: why the fuck is this hardcoded, there should be a default, but it should be a
        // static and it should be overidable
        if let Some(dns) = ctx
            .dns
            .filter(|value| !value.is_empty())
            .or(Some("1.1.1.1"))
        {
            command.arg("--dns").arg(dns);
        }
        Self {
            ctx,
            command,
            workspace_mount,
        }
    }

    fn with_volumes(mut self) -> Self {
        for mount in self.ctx.mounts {
            self.command.arg("--volume").arg(mount.to_arg());
        }
        self
    }

    fn with_workspace_volume(mut self) -> Self {
        self.command.arg("--volume").arg(&self.workspace_mount);
        self
    }

    fn with_network(mut self) -> Self {
        if let Some(network) = self.ctx.network {
            self.command.arg("--network").arg(network);
        }
        self
    }

    fn with_image_options(mut self) -> Self {
        if let Some(user) = self.ctx.image_user {
            self.command.arg("--user").arg(user);
        }
        if !self.ctx.image_entrypoint.is_empty() {
            self.command
                .arg("--entrypoint")
                .arg(self.ctx.image_entrypoint.join(" "));
        }
        self
    }

    fn with_env(mut self) -> Self {
        for (key, value) in self.ctx.env_vars {
            self.command.arg("--env").arg(format!("{key}={value}"));
        }
        self
    }

    fn build(mut self) -> Command {
        self.command
            .arg(self.ctx.image)
            .arg("sh")
            .arg(self.ctx.container_script);
        if std::env::var("OPAL_DEBUG_CONTAINER")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            let program = self.command.get_program().to_string_lossy();
            let args: Vec<String> = self
                .command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect();
            eprintln!("[opal] container command: {} {}", program, args.join(" "));
        }
        self.command
    }
}

#[cfg(test)]
mod tests {
    use super::ContainerExecutor;
    use crate::engine::EngineCommandContext;
    use crate::executor::container_arch::{container_arch_from_platform, normalize_container_arch};
    use crate::pipeline::VolumeMount;
    use std::path::Path;

    #[test]
    fn build_command_uses_rm_for_job_containers() {
        let ctx = EngineCommandContext {
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
        };

        let command = ContainerExecutor::build_command(&ctx);
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert!(args.iter().any(|arg| arg == "--rm"));
    }

    #[test]
    fn build_command_skips_rm_when_preserving_runtime_objects() {
        let ctx = EngineCommandContext {
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
            preserve_runtime_objects: true,
            arch: None,
            privileged: false,
            cap_add: &[],
            cap_drop: &[],
            cpus: None,
            memory: None,
            dns: None,
        };

        let args: Vec<String> = ContainerExecutor::build_command(&ctx)
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert!(!args.iter().any(|arg| arg == "--rm"));
    }

    #[test]
    fn build_command_mounts_workspace_before_nested_artifacts() {
        let mounts = [VolumeMount {
            host: "/tmp/artifacts".into(),
            container: "/builds/workspace/tests-temp/shared".into(),
            read_only: true,
        }];
        let ctx = EngineCommandContext {
            workdir: Path::new("/workspace"),
            container_root: Path::new("/builds/workspace"),
            container_script: Path::new("/opal/script.sh"),
            container_name: "opal-job",
            image: "alpine:3.19",
            image_platform: None,
            image_user: None,
            image_entrypoint: &[],
            mounts: &mounts,
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
        };

        let args: Vec<String> = ContainerExecutor::build_command(&ctx)
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
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
    fn normalize_container_arch_maps_apple_silicon_name() {
        assert_eq!(
            normalize_container_arch("aarch64").as_deref(),
            Some("arm64")
        );
        assert_eq!(
            normalize_container_arch("x86_64").as_deref(),
            Some("x86_64")
        );
    }

    #[test]
    fn container_arch_from_platform_maps_common_linux_platforms() {
        assert_eq!(
            container_arch_from_platform("linux/arm64/v8").as_deref(),
            Some("arm64")
        );
        assert_eq!(
            container_arch_from_platform("linux/amd64").as_deref(),
            Some("x86_64")
        );
    }

    #[test]
    fn build_command_prefers_image_platform_over_host_default() {
        let ctx = EngineCommandContext {
            workdir: Path::new("/workspace"),
            container_root: Path::new("/builds/workspace"),
            container_script: Path::new("/opal/script.sh"),
            container_name: "opal-job",
            image: "alpine:3.19",
            image_platform: Some("linux/amd64"),
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
        };

        let args: Vec<String> = ContainerExecutor::build_command(&ctx)
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert!(args.windows(2).any(|pair| pair == ["--arch", "x86_64"]));
    }

    #[test]
    fn build_command_includes_image_user_and_entrypoint() {
        let entrypoint = vec!["/bin/sh".to_string(), "-lc".to_string()];
        let ctx = EngineCommandContext {
            workdir: Path::new("/workspace"),
            container_root: Path::new("/builds/workspace"),
            container_script: Path::new("/opal/script.sh"),
            container_name: "opal-job",
            image: "alpine:3.19",
            image_platform: None,
            image_user: Some("1000:1000"),
            image_entrypoint: &entrypoint,
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
        };

        let args: Vec<String> = ContainerExecutor::build_command(&ctx)
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert!(args.windows(2).any(|pair| pair == ["--user", "1000:1000"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--entrypoint", "/bin/sh -lc"])
        );
    }
}
