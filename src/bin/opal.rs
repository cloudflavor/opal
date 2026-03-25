use anyhow::{Context, Result};
use opal::compiler::compile_pipeline;
use opal::config::OpalConfig;
use opal::display::{self, DisplayFormatter};
use opal::execution_plan::build_execution_plan;
use opal::executor::{
    ContainerExecutor, DockerExecutor, NerdctlExecutor, OrbstackExecutor, PodmanExecutor,
};
use opal::logging;
use opal::model::PipelineSpec;
use opal::pipeline::{self, RuleContext};
use opal::secrets::SecretsStore;
use opal::terminal;
use opal::ui;
use opal::{
    Cli, Commands, EngineChoice, EngineKind, ExecutorConfig, GitLabRemoteConfig, PlanArgs, RunArgs,
    ViewArgs, runtime,
};
use std::env;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;
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
    let current_dir = env::current_dir()?;

    match opts.commands {
        Commands::Run(args) => {
            let RunArgs {
                pipeline,
                workdir,
                base_image,
                env_includes,
                max_parallel_jobs,
                trace_scripts,
                engine,
                no_tui,
                gitlab_base_url,
                gitlab_token,
                jobs,
            } = args;
            let resolved_workdir = workdir.unwrap_or(current_dir);
            let resolved_pipeline =
                pipeline.unwrap_or_else(|| resolved_workdir.join(".gitlab-ci.yml"));
            let settings = OpalConfig::load(&resolved_workdir)?;

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
                selected_jobs: jobs,
                max_parallel_jobs,
                enable_tui: !no_tui,
                engine: engine_kind,
                gitlab,
                settings,
                trace_scripts,
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
        Commands::Plan(args) => run_plan(args),
        Commands::View(args) => run_view(args),
    }
}

fn run_view(args: ViewArgs) -> Result<()> {
    let workdir = match args.workdir {
        Some(path) => path,
        None => env::current_dir().with_context(|| "failed to determine current dir")?,
    };
    ui::view_pipeline_logs(&workdir)
}

fn run_plan(args: PlanArgs) -> Result<()> {
    let workdir = match args.workdir {
        Some(path) => path,
        None => env::current_dir().with_context(|| "failed to determine current dir")?,
    };
    let pipeline = args
        .pipeline
        .unwrap_or_else(|| workdir.join(".gitlab-ci.yml"));
    let gitlab = args.gitlab_token.map(|token| GitLabRemoteConfig {
        base_url: args
            .gitlab_base_url
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| "https://gitlab.com".to_string()),
        token,
    });
    let pipeline_spec = PipelineSpec::from_path_with_gitlab(&pipeline, gitlab.as_ref())
        .with_context(|| format!("failed to load pipeline {}", pipeline.display()))?;
    let ctx = rule_context_for_workdir(&workdir);
    ctx.ensure_valid_tag_context()?;
    if !pipeline::rules::filters_allow(&pipeline_spec.filters, &ctx) {
        // TODO: should be a warn! from tracing
        println!("pipeline skipped: top-level only/except filters exclude this ref");
        return Ok(());
    }
    if let Some(workflow) = &pipeline_spec.workflow
        && !pipeline::rules::evaluate_workflow(&workflow.rules, &ctx)?
    {
        println!("pipeline skipped: workflow rules excluded this run");
        return Ok(());
    }

    let logs_dir = runtime::runs_root().join("plan/logs");
    fs::create_dir_all(&logs_dir)
        .with_context(|| format!("failed to create plan log dir {}", logs_dir.display()))?;
    let compiled = compile_pipeline(&pipeline_spec, Some(&ctx))?;
    let mut plan = build_execution_plan(compiled, |job| {
        logging::job_log_info(&logs_dir, "plan-preview", job)
    })?;
    if !args.jobs.is_empty() {
        plan = plan.select_jobs(&args.jobs)?;
    }
    let display = DisplayFormatter::new(terminal::should_use_color());
    display::print_pipeline_plan(&display, &plan, display::print_line);
    Ok(())
}

fn rule_context_for_workdir(workdir: &Path) -> RuleContext {
    let mut ctx_env: std::collections::HashMap<String, String> = env::vars().collect();
    let run_manual = env::var("OPAL_RUN_MANUAL").is_ok_and(|v| v == "1");
    if let Ok(secrets) = SecretsStore::load(workdir) {
        ctx_env.extend(secrets.env_pairs());
    }
    RuleContext::from_env(workdir, ctx_env, run_manual)
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

#[cfg(test)]
mod tests {
    use super::rule_context_for_workdir;
    use anyhow::Result;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn rule_context_includes_opal_env_values() -> Result<()> {
        let dir = tempdir()?;
        let secrets_dir = dir.path().join(".opal").join("env");
        fs::create_dir_all(&secrets_dir)?;
        fs::write(secrets_dir.join("QUAY_USERNAME"), "robot-user")?;

        let ctx = rule_context_for_workdir(dir.path());
        assert_eq!(ctx.env_value("QUAY_USERNAME"), Some("robot-user"));
        Ok(())
    }
}
