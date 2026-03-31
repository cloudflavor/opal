use super::OpalApp;
use super::context::{
    resolve_engine, resolve_engine_choice, resolve_gitlab_remote, resolve_pipeline_path,
    validate_engine_choice,
};
use crate::config::OpalConfig;
use crate::executor::{
    ContainerExecutor, DockerExecutor, NerdctlExecutor, OrbstackExecutor, PodmanExecutor,
};
use crate::history::{self, HistoryEntry};
use crate::runtime;
use crate::{EngineKind, ExecutorConfig, RunArgs};
use anyhow::{Context, Result};
use std::collections::HashSet;

pub(crate) async fn execute(app: &OpalApp, args: RunArgs) -> Result<()> {
    let config = build_executor_config(app, args, true)?;
    execute_with_config(config).await
}

#[derive(Debug, Clone)]
pub(crate) struct RunCapture {
    pub history_entry: Option<HistoryEntry>,
    pub error: Option<String>,
}

pub(crate) async fn execute_and_capture(app: &OpalApp, args: RunArgs) -> RunCapture {
    let before = history::load(&runtime::history_path()).unwrap_or_default();
    let known_run_ids = before
        .iter()
        .map(|entry| entry.run_id.clone())
        .collect::<HashSet<_>>();
    let result = match build_executor_config(app, args, false) {
        Ok(config) => execute_with_config(config).await,
        Err(err) => Err(err),
    };
    let after = history::load(&runtime::history_path()).unwrap_or_default();
    let history_entry = after
        .iter()
        .rev()
        .find(|entry| !known_run_ids.contains(&entry.run_id))
        .cloned()
        .or_else(|| after.last().cloned());

    RunCapture {
        history_entry,
        error: result.err().map(|err| err.to_string()),
    }
}

fn build_executor_config(
    app: &OpalApp,
    args: RunArgs,
    emit_console_output: bool,
) -> Result<ExecutorConfig> {
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

    let resolved_workdir = app.resolve_workdir(workdir);
    let resolved_pipeline = resolve_pipeline_path(&resolved_workdir, pipeline);
    let settings = OpalConfig::load(&resolved_workdir)?;
    let engine = resolve_engine_choice(engine, &settings);
    validate_engine_choice(engine)?;
    let engine_kind = resolve_engine(engine);
    let gitlab = resolve_gitlab_remote(gitlab_base_url, gitlab_token);

    Ok(ExecutorConfig {
        image: base_image,
        workdir: resolved_workdir,
        pipeline: resolved_pipeline,
        env_includes,
        selected_jobs: jobs,
        max_parallel_jobs,
        enable_tui: !no_tui,
        emit_console_output,
        engine: engine_kind,
        gitlab,
        settings,
        trace_scripts,
    })
}

async fn execute_with_config(config: ExecutorConfig) -> Result<()> {
    let engine_kind = config.engine;
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
