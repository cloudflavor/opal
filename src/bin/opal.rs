use anyhow::Result;
#[cfg(target_os = "macos")]
use opal::executor::ContainerExecutor;
#[cfg(target_os = "linux")]
use opal::executor::PDExecutor;
use opal::{Cli, Commands};
use structopt::StructOpt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::Subscriber;

#[tokio::main]
async fn main() -> Result<()> {
    let opts = Cli::from_args();

    let opts_level = opts.log_level;
    let env_filter = EnvFilter::new(opts_level.as_str());

    let subscriber = Subscriber::builder()
        .with_ansi(true)
        .with_env_filter(env_filter)
        .with_writer(std::io::stdout)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    match opts.commands {
        Commands::Run(args) => {
            #[cfg(target_os = "linux")]
            let _nerd_executor = PDExecutor::new(args.base_image, args.workdir);

            #[cfg(target_os = "macos")]
            let _container_executor = ContainerExecutor::new(args.base_image, args.workdir);
        }
    }

    Ok(())
}
