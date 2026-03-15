use anyhow::{Context, Result};
use opal::executor::{
    ContainerExecutor, DockerExecutor, NerdctlExecutor, OrbstackExecutor, PodmanExecutor,
};
use opal::ui;
use opal::{
    Cli, Commands, EngineChoice, EngineKind, ExecutorConfig, GitLabRemoteConfig, RunArgs, ViewArgs,
};
use std::env;
use std::io::{self, IsTerminal};
use structopt::StructOpt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::Subscriber;

#[tokio::main]
async fn main() -> Result<()> {
    let opts = Cli::from_args();

    let opts_level = opts.log_level;
    let env_filter = EnvFilter::new(opts_level.as_str());

    let use_ansi = io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();

    let subscriber = Subscriber::builder()
        .with_ansi(use_ansi)
        .with_env_filter(env_filter)
        .with_writer(std::io::stdout)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    match opts.commands {
        Commands::Run(args) => {
            let RunArgs {
                pipeline,
                workdir,
                base_image,
                env_includes,
                max_parallel_jobs,
                log_dir,
                engine,
                no_tui,
                gitlab_base_url,
                gitlab_token,
            } = args;
            let resolved_workdir = workdir
                .unwrap_or_else(|| env::current_dir().expect("failed to determine current dir"));
            let resolved_pipeline =
                pipeline.unwrap_or_else(|| resolved_workdir.join(".gitlab-ci.yml"));

            let engine_kind = resolve_engine(engine);
            let gitlab = gitlab_token.map(|token| GitLabRemoteConfig {
                base_url: gitlab_base_url
                    .filter(|url| !url.is_empty())
                    .unwrap_or_else(|| "https://gitlab.com".to_string()),
                token,
            });
            let config = ExecutorConfig {
                image: base_image,
                workdir: resolved_workdir,
                pipeline: resolved_pipeline,
                env_includes,
                max_parallel_jobs,
                log_dir,
                enable_tui: !no_tui,
                engine: engine_kind,
                gitlab,
            };

            let run_result = match engine_kind {
                EngineKind::ContainerCli => {
                    let executor = ContainerExecutor::new(config.clone())
                        .with_context(|| "failed create container executor")?;
                    executor.run().await
                }
                EngineKind::Docker => {
                    let executor = DockerExecutor::new(config.clone())
                        .with_context(|| "failed create docker executor")?;
                    executor.run().await
                }
                EngineKind::Podman => {
                    let executor = PodmanExecutor::new(config.clone())
                        .with_context(|| "failed create podman executor")?;
                    executor.run().await
                }
                EngineKind::Nerdctl => {
                    let executor = NerdctlExecutor::new(config.clone())
                        .with_context(|| "failed create nerdctl executor")?;
                    executor.run().await
                }
                EngineKind::Orbstack => {
                    let executor = OrbstackExecutor::new(config.clone())
                        .with_context(|| "failed create orbstack executor")?;
                    executor.run().await
                }
            };

            run_result.with_context(|| "failed to run pipeline")
        }
        Commands::View(args) => run_view(args),
    }
}

fn run_view(args: ViewArgs) -> Result<()> {
    let workdir = args
        .workdir
        .unwrap_or_else(|| env::current_dir().expect("failed to determine current dir"));
    ui::view_pipeline_logs(&workdir)
}

#[cfg(target_os = "macos")]
fn resolve_engine(choice: EngineChoice) -> EngineKind {
    match choice {
        EngineChoice::Auto => {
            if detect_orbstack() {
                EngineKind::Orbstack
            } else {
                EngineKind::ContainerCli
            }
        }
        EngineChoice::Container => EngineKind::ContainerCli,
        EngineChoice::Docker => EngineKind::Docker,
        EngineChoice::Orbstack => EngineKind::Orbstack,
        EngineChoice::Podman => EngineKind::Podman,
        EngineChoice::Nerdctl => EngineKind::Nerdctl,
    }
}

#[cfg(target_os = "macos")]
fn detect_orbstack() -> bool {
    if std::env::var_os("ORBSTACK").is_some() {
        return true;
    }
    if let Some(host) = std::env::var_os("DOCKER_HOST")
        && let Ok(host_str) = host.into_string()
        && host_str.contains(".orbstack")
    {
        return true;
    }
    false
}

#[cfg(target_os = "linux")]
fn resolve_engine(choice: EngineChoice) -> EngineKind {
    match choice {
        EngineChoice::Auto | EngineChoice::Docker => EngineKind::Docker,
        EngineChoice::Podman => EngineKind::Podman,
        EngineChoice::Nerdctl => EngineKind::Nerdctl,
        EngineChoice::Orbstack => EngineKind::Docker,
        EngineChoice::Container => {
            eprintln!("'container' engine is unavailable on Linux; falling back to docker");
            EngineKind::Docker
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resolve_engine(_: EngineChoice) -> EngineKind {
    EngineKind::Docker
}
