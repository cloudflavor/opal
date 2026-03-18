use std::path::PathBuf;
use std::str::FromStr;

use structopt::StructOpt;

pub mod config;
pub mod display;
pub mod engine;
pub mod env;
pub mod executor;
pub mod git;
pub mod gitlab;
pub mod history;
pub mod logging;
pub mod naming;
pub mod pipeline;
pub mod runner;
pub mod runtime;
pub mod secrets;
pub mod terminal;
pub mod ui;

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
    Plan(PlanArgs),
    View(ViewArgs),
}

#[derive(StructOpt)]
pub struct RunArgs {
    /// Which .gitlab-ci.yml file to use (defaults to <workdir>/.gitlab-ci.yml)
    #[structopt(short, long)]
    pub pipeline: Option<PathBuf>,

    #[structopt(short, long)]
    /// Context directory (defaults to current working directory)
    pub workdir: Option<PathBuf>,

    #[structopt(short, long)]
    /// Optional base image override; falls back to pipeline/job image.
    pub base_image: Option<String>,

    #[structopt(
        short = "E",
        long = "env",
        value_name = "GLOB",
        help = "Include host env vars matching this glob (e.g. APP_*). Repeat to add more."
    )]
    pub env_includes: Vec<String>,

    #[structopt(long = "max-parallel-jobs", default_value = "5")]
    /// Maximum number of jobs to run concurrently
    pub max_parallel_jobs: usize,

    #[structopt(long = "trace-scripts")]
    /// Print each job command as it runs (enables shell `set -x`)
    pub trace_scripts: bool,

    #[structopt(
        long = "engine",
        default_value = "auto",
        possible_values = EngineChoice::VARIANTS,
        help = "Container runtime to use (auto, container, docker, podman, nerdctl, orbstack)"
    )]
    pub engine: EngineChoice,

    #[structopt(long = "no-tui")]
    /// Disable the Ratatui interface
    pub no_tui: bool,

    #[structopt(long = "gitlab-base-url", env = "OPAL_GITLAB_BASE_URL")]
    /// Base URL for GitLab API (default: https://gitlab.com)
    pub gitlab_base_url: Option<String>,

    #[structopt(long = "gitlab-token", env = "OPAL_GITLAB_TOKEN")]
    /// Personal access token used when downloading cross-project artifacts
    pub gitlab_token: Option<String>,
}

#[derive(StructOpt)]
pub struct ViewArgs {
    #[structopt(short, long)]
    /// Context directory (defaults to current working directory)
    pub workdir: Option<PathBuf>,
}

#[derive(StructOpt)]
pub struct PlanArgs {
    /// Which .gitlab-ci.yml file to inspect (defaults to <workdir>/.gitlab-ci.yml)
    #[structopt(short, long)]
    pub pipeline: Option<PathBuf>,

    #[structopt(short, long)]
    /// Context directory (defaults to current working directory)
    pub workdir: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
pub enum EngineChoice {
    Auto,
    Container,
    Docker,
    Podman,
    Nerdctl,
    Orbstack,
}

impl EngineChoice {
    pub const VARIANTS: &'static [&'static str] = &[
        "auto",
        "container",
        "docker",
        "podman",
        "nerdctl",
        "orbstack",
    ];
}

impl FromStr for EngineChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "container" => Ok(Self::Container),
            "docker" => Ok(Self::Docker),
            "podman" => Ok(Self::Podman),
            "nerdctl" => Ok(Self::Nerdctl),
            "orbstack" => Ok(Self::Orbstack),
            other => Err(format!("unknown engine '{other}'")),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum EngineKind {
    ContainerCli,
    Docker,
    Podman,
    Nerdctl,
    Orbstack,
}

#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    pub image: Option<String>,
    pub workdir: PathBuf,
    pub pipeline: PathBuf,
    pub env_includes: Vec<String>,
    pub max_parallel_jobs: usize,
    pub enable_tui: bool,
    pub engine: EngineKind,
    pub gitlab: Option<GitLabRemoteConfig>,
    pub settings: config::OpalConfig,
    pub trace_scripts: bool,
}

#[derive(Clone, Debug)]
pub struct GitLabRemoteConfig {
    pub base_url: String,
    pub token: String,
}
