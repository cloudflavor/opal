use anyhow::{Context, Result};
use opal::executor::ContainerExecutor;
use opal::{Cli, Commands, EngineChoice, EngineKind, ExecutorConfig, RunArgs};
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
            } = args;

            let container_executor = ContainerExecutor::new(ExecutorConfig {
                image: base_image,
                workdir,
                pipeline,
                env_includes,
                max_parallel_jobs,
                log_dir,
                enable_tui: !no_tui,
                engine: resolve_engine(engine),
            })
            .with_context(|| "failed create new executor")?;

            container_executor
                .run()
                .await
                .with_context(|| "failed to run pipeline")
        }
    }
}

#[cfg(target_os = "macos")]
fn resolve_engine(choice: EngineChoice) -> EngineKind {
    match choice {
        EngineChoice::Auto | EngineChoice::Container => EngineKind::ContainerCli,
        EngineChoice::Docker => EngineKind::Docker,
        EngineChoice::Podman => EngineKind::Podman,
        EngineChoice::Nerdctl => EngineKind::Nerdctl,
    }
}

#[cfg(target_os = "linux")]
fn resolve_engine(choice: EngineChoice) -> EngineKind {
    match choice {
        EngineChoice::Auto | EngineChoice::Docker => EngineKind::Docker,
        EngineChoice::Podman => EngineKind::Podman,
        EngineChoice::Nerdctl => EngineKind::Nerdctl,
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
