use std::path::PathBuf;

use structopt::StructOpt;

pub mod executor;
pub mod pipeline;

#[derive(StructOpt)]
pub struct Cli {
    #[structopt(
        short,
        long,
        default_value = "info",
        possible_values = &["trace", "debug", "info", "warn", "error"]
    )]
    pub log_level: tracing::Level,

    #[structopt(subcommand)]
    pub commands: Commands,
}

#[derive(StructOpt)]
pub enum Commands {
    Run(RunArgs),
}

#[derive(StructOpt)]
pub struct RunArgs {
    /// Which .gitlab-ci.yml file to use.
    /// Defaults to current working directory
    #[structopt(short, long, default_value = ".gitlab-ci.yml")]
    pub pipeline: PathBuf,

    #[structopt(short, long)]
    /// Context directory
    pub workdir: PathBuf,

    #[structopt(short, long)]
    /// The base image that the runner should use.
    /// Overrides image specified in the .gitlab-ci.yml file
    pub base_image: String,

    #[structopt(
        short = "E",
        long = "env",
        value_name = "GLOB",
        help = "Include host env vars matching this glob (e.g. APP_*). Repeat to add more."
    )]
    pub env_includes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    pub image: String,
    pub workdir: PathBuf,
    pub pipeline: PathBuf,
    pub env_includes: Vec<String>,
}
