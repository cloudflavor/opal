use serde::Deserialize;
use std::path::PathBuf;
use std::str::FromStr;
use structopt::StructOpt;

pub mod ai;
pub mod app;
pub mod compiler;
pub mod config;
pub mod display;
pub mod engine;
pub mod env;
pub mod execution_plan;
pub mod executor;
pub mod git;
pub mod gitlab;
pub mod history;
pub mod logging;
pub mod mcp;
pub mod model;
pub mod naming;
pub mod pipeline;
pub mod runner;
pub mod runtime;
pub mod secrets;
pub mod terminal;
pub mod ui;

/// Terminal-first GitLab pipeline runner for local debugging.
///
/// Opal can evaluate `.gitlab-ci.yml`, render the local execution plan,
/// run selected jobs in local containers, browse run history, and expose
/// an MCP server for editor and agent integrations.
#[derive(StructOpt)]
#[structopt(name = "opal", version = env!("OPAL_BUILD_VERSION"))]
pub struct Cli {
    /// Logging verbosity for Opal itself.
    ///
    /// This affects Opal's own tracing output rather than the log content
    /// emitted by pipeline jobs.
    #[structopt(
        short,
        long,
        default_value = "info",
        possible_values = &["trace", "debug", "info", "warn", "error"]
    )]
    pub log_level: tracing::Level,

    /// Which Opal command to run.
    #[structopt(subcommand)]
    pub commands: Commands,
}

#[derive(StructOpt)]
pub enum Commands {
    /// Run a local pipeline execution.
    Run(RunArgs),
    /// Render the evaluated execution plan without starting containers.
    Plan(PlanArgs),
    /// Open the history and log browser for previous runs.
    View(ViewArgs),
    /// List active and recent background operations.
    Operations(OperationsArgs),
    /// Print a shell completion script to stdout.
    Completions(CompletionArgs),
    /// Start the MCP server over stdio.
    Mcp(McpArgs),
}

/// Start the MCP server over stdio.
#[derive(StructOpt, Default)]
#[structopt(name = "mcp", bin_name = "opal mcp")]
pub struct McpArgs {}

#[cfg(test)]
mod cli_tests {
    use super::{Cli, Commands, CompletionShell, McpArgs, OperationStatusFilter};
    use structopt::StructOpt;

    #[test]
    fn mcp_subcommand_app_can_override_bin_name() {
        let app = McpArgs::clap().bin_name("opal mcp");
        let mut help = Vec::new();
        app.write_help(&mut help).expect("write help");
        let text = String::from_utf8(help).expect("utf8");
        assert!(text.contains("opal mcp"));
    }

    #[test]
    fn completion_shell_parses_case_insensitively() {
        assert_eq!(
            "ZSH".parse::<CompletionShell>().expect("shell"),
            CompletionShell::Zsh
        );
    }

    #[test]
    fn completions_subcommand_parses_shell_argument() {
        let cli = Cli::from_iter_safe(["opal", "completions", "fish"]).expect("parse cli");
        match cli.commands {
            Commands::Completions(args) => assert_eq!(args.shell, CompletionShell::Fish),
            _ => panic!("expected completions subcommand"),
        }
    }

    #[test]
    fn operations_subcommand_parses_filters() {
        let cli = Cli::from_iter_safe([
            "opal",
            "operations",
            "--all",
            "--status",
            "failed",
            "--limit",
            "5",
            "--json",
        ])
        .expect("parse cli");
        match cli.commands {
            Commands::Operations(args) => {
                assert!(args.all);
                assert_eq!(args.status, Some(OperationStatusFilter::Failed));
                assert_eq!(args.limit, 5);
                assert!(args.json);
            }
            _ => panic!("expected operations subcommand"),
        }
    }
}

/// Run a local pipeline execution.
#[derive(StructOpt)]
pub struct RunArgs {
    /// Which `.gitlab-ci.yml` file to use.
    ///
    /// Defaults to `<workdir>/.gitlab-ci.yml`.
    #[structopt(short, long)]
    pub pipeline: Option<PathBuf>,

    /// Context directory for the pipeline run.
    ///
    /// Defaults to the current working directory.
    #[structopt(short, long)]
    pub workdir: Option<PathBuf>,

    /// Optional fallback image for jobs that do not declare one.
    ///
    /// Job-level and pipeline-level images still take precedence.
    #[structopt(short, long)]
    pub base_image: Option<String>,

    /// Include matching host environment variables in every job.
    ///
    /// Repeat this option to add multiple glob patterns.
    #[structopt(
        short = "E",
        long = "env",
        value_name = "GLOB",
        help = "Include host env vars matching this glob (e.g. APP_*). Repeat to add more."
    )]
    pub env_includes: Vec<String>,

    /// Maximum number of jobs to run at the same time.
    #[structopt(long = "max-parallel-jobs", default_value = "5")]
    pub max_parallel_jobs: usize,

    /// Print each generated job command as it executes.
    ///
    /// This enables shell tracing with `set -x` in generated scripts.
    #[structopt(long = "trace-scripts")]
    pub trace_scripts: bool,

    /// Container engine to use for local execution.
    ///
    /// `auto` picks the platform default. On macOS that is typically
    /// Apple `container`; on Linux it is typically `podman`.
    #[structopt(
        long = "engine",
        default_value = "auto",
        possible_values = EngineChoice::VARIANTS,
        help = "Container runtime to use (auto, container, docker, podman, nerdctl, orbstack). nerdctl is Linux-specific in Opal."
    )]
    pub engine: EngineChoice,

    /// Disable the interactive terminal UI.
    ///
    /// Opal still executes the pipeline, but prints plain terminal output
    /// instead of opening the Ratatui interface.
    #[structopt(long = "no-tui")]
    pub no_tui: bool,

    /// Base URL for GitLab API access.
    ///
    /// Used when resolving GitLab-backed includes or artifacts. Defaults to
    /// `https://gitlab.com` when paired with token-based GitLab features.
    #[structopt(long = "gitlab-base-url", env = "OPAL_GITLAB_BASE_URL")]
    pub gitlab_base_url: Option<String>,

    /// Personal access token for GitLab-backed features.
    ///
    /// Used for cross-project artifacts and include resolution that require
    /// GitLab API access.
    #[structopt(long = "gitlab-token", env = "OPAL_GITLAB_TOKEN")]
    pub gitlab_token: Option<String>,

    /// Rerun a job name from the latest or selected recorded run.
    ///
    /// When set, Opal verifies the job existed in recorded history and then
    /// reruns that job name against the current checkout.
    #[structopt(long = "rerun-job", value_name = "NAME")]
    pub rerun_job: Option<String>,

    /// Recorded run to use with `--rerun-job`.
    ///
    /// Defaults to the latest recorded run when omitted.
    #[structopt(long = "rerun-run-id", value_name = "RUN_ID")]
    pub rerun_run_id: Option<String>,

    /// Limit execution to selected jobs and their required upstream closure.
    ///
    /// Repeat this option to select multiple jobs.
    #[structopt(long = "job", value_name = "NAME")]
    pub jobs: Vec<String>,
}

/// Open the history and log browser for previous runs.
#[derive(StructOpt)]
pub struct ViewArgs {
    /// Context directory whose Opal state should be inspected.
    ///
    /// Defaults to the current working directory.
    #[structopt(short, long)]
    pub workdir: Option<PathBuf>,
}

/// Print a shell completion script to stdout.
#[derive(StructOpt)]
pub struct CompletionArgs {
    /// Which shell to generate completions for.
    #[structopt(
        value_name = "SHELL",
        possible_values = CompletionShell::VARIANTS,
        case_insensitive = true
    )]
    pub shell: CompletionShell,
}

/// Render the evaluated execution plan without starting containers.
#[derive(StructOpt)]
pub struct PlanArgs {
    /// Which `.gitlab-ci.yml` file to inspect.
    ///
    /// Defaults to `<workdir>/.gitlab-ci.yml`.
    #[structopt(short, long)]
    pub pipeline: Option<PathBuf>,

    /// Context directory for pipeline evaluation.
    ///
    /// Defaults to the current working directory.
    #[structopt(short, long)]
    pub workdir: Option<PathBuf>,

    /// Base URL for GitLab API access during plan evaluation.
    #[structopt(long = "gitlab-base-url", env = "OPAL_GITLAB_BASE_URL")]
    pub gitlab_base_url: Option<String>,

    /// Personal access token for GitLab-backed include resolution.
    #[structopt(long = "gitlab-token", env = "OPAL_GITLAB_TOKEN")]
    pub gitlab_token: Option<String>,

    /// Limit planning to selected jobs and their required upstream closure.
    ///
    /// Repeat this option to select multiple jobs.
    #[structopt(long = "job", value_name = "NAME")]
    pub jobs: Vec<String>,

    /// Print the plan directly instead of opening a pager.
    #[structopt(long = "no-pager")]
    pub no_pager: bool,

    /// Emit the execution plan as JSON.
    #[structopt(long = "json")]
    pub json: bool,
}

/// List active and recent background operations.
#[derive(StructOpt)]
pub struct OperationsArgs {
    /// Include completed and failed operations, not only running ones.
    #[structopt(long)]
    pub all: bool,

    /// Filter operations by status.
    #[structopt(long, possible_values = OperationStatusFilter::VARIANTS)]
    pub status: Option<OperationStatusFilter>,

    /// Maximum number of operations to return.
    #[structopt(long, default_value = "20")]
    pub limit: usize,

    /// Print operation data as JSON.
    #[structopt(long)]
    pub json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationStatusFilter {
    Running,
    Succeeded,
    Failed,
}

impl OperationStatusFilter {
    pub const VARIANTS: &'static [&'static str] = &["running", "succeeded", "failed"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for OperationStatusFilter {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            other => Err(format!("unsupported operation status '{other}'")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionShell {
    Bash,
    Elvish,
    Fish,
    PowerShell,
    Zsh,
}

impl CompletionShell {
    pub const VARIANTS: &'static [&'static str] = &["bash", "elvish", "fish", "powershell", "zsh"];

    pub fn to_clap_shell(self) -> structopt::clap::Shell {
        match self {
            Self::Bash => structopt::clap::Shell::Bash,
            Self::Elvish => structopt::clap::Shell::Elvish,
            Self::Fish => structopt::clap::Shell::Fish,
            Self::PowerShell => structopt::clap::Shell::PowerShell,
            Self::Zsh => structopt::clap::Shell::Zsh,
        }
    }
}

impl FromStr for CompletionShell {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "bash" => Ok(Self::Bash),
            "elvish" => Ok(Self::Elvish),
            "fish" => Ok(Self::Fish),
            "powershell" => Ok(Self::PowerShell),
            "zsh" => Ok(Self::Zsh),
            _ => Err(format!(
                "unsupported shell '{s}'; expected one of: {}",
                Self::VARIANTS.join(", ")
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
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
    pub selected_jobs: Vec<String>,
    pub max_parallel_jobs: usize,
    pub enable_tui: bool,
    pub emit_console_output: bool,
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
