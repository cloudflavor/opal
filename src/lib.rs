use std::path::PathBuf;

use structopt::StructOpt;

pub mod executor;

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
    #[structopt(short, long)]
    pub workdir: PathBuf,

    #[structopt(short, long)]
    pub base_image: String,
}
