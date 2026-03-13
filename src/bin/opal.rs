#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::Result;
#[cfg(target_os = "macos")]
use opal::executor::ContainerExecutor;
#[cfg(target_os = "linux")]
use opal::executor::PDExecutor;
use opal::{Cli, Commands, ExecutorConfig, RunArgs};
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
            #[cfg(target_os = "linux")]
            let _nerd_executor = PDExecutor::new(ExecutorConfig {});

            #[cfg(target_os = "macos")]
            let container_executor = {
                let RunArgs {
                    pipeline,
                    workdir,
                    base_image,
                    env_includes,
                    max_parallel_jobs,
                    log_dir,
                    no_tui,
                } = args;

                ContainerExecutor::new(ExecutorConfig {
                    image: base_image,
                    workdir,
                    pipeline,
                    env_includes,
                    max_parallel_jobs,
                    log_dir,
                    enable_tui: !no_tui,
                })
                .with_context(|| "failed create new exeecutor")?
            };

            container_executor
                .run()
                .await
                .with_context(|| "failed to run pipeline")
        }
    }
}
