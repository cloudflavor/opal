use super::OpalApp;
use super::context::{
    resolve_engine, resolve_engine_choice, resolve_gitlab_remote, resolve_pipeline_path,
    validate_engine_choice,
};
use super::view::{find_history_entry_for_workdir, find_job, latest_history_entry_for_workdir};
use crate::config::OpalConfig;
use crate::executor::{
    ContainerExecutor, DockerExecutor, NerdctlExecutor, OrbstackExecutor, PodmanExecutor,
    core::ExecutionOutcome,
};
use crate::history::HistoryEntry;
use crate::{EngineKind, ExecutorConfig, RunArgs};
use anyhow::{Context, Result};

pub(crate) async fn execute(app: &OpalApp, args: RunArgs) -> Result<()> {
    let config = build_executor_config(app, prepare_run_args(app, args)?, true)?;
    execute_with_config(config).await.result
}

#[derive(Debug, Clone)]
pub(crate) struct RunCapture {
    pub history_entry: Option<HistoryEntry>,
    pub result: Option<serde_json::Value>,
    pub result_summary: Option<String>,
    pub error: Option<String>,
}

pub(crate) async fn execute_and_capture(app: &OpalApp, args: RunArgs) -> RunCapture {
    let prepared = match prepare_run_args(app, args)
        .and_then(|args| build_executor_config(app, args, false))
    {
        Ok(config) => config,
        Err(err) => {
            return RunCapture {
                history_entry: None,
                result: None,
                result_summary: None,
                error: Some(err.to_string()),
            };
        }
    };
    run_capture_from_outcome(execute_with_config(prepared).await)
}

fn prepare_run_args(app: &OpalApp, mut args: RunArgs) -> Result<RunArgs> {
    let resolved_workdir = app.resolve_workdir(args.workdir.clone());
    match (&args.rerun_job, &args.rerun_run_id) {
        (None, Some(_)) => anyhow::bail!("--rerun-run-id requires --rerun-job"),
        (Some(_), _) if !args.jobs.is_empty() => {
            anyhow::bail!("--rerun-job cannot be combined with --job")
        }
        (None, None) => return Ok(args),
        (Some(job_name), _) => {
            let entry = match args.rerun_run_id.as_deref() {
                Some(run_id) => find_history_entry_for_workdir(&resolved_workdir, run_id)?
                    .with_context(|| format!("run '{run_id}' not found in Opal history"))?,
                None => latest_history_entry_for_workdir(&resolved_workdir)?
                    .context("no Opal history entries found")?,
            };
            find_job(&entry, job_name).with_context(|| {
                format!(
                    "job '{job_name}' not found in recorded run '{}'",
                    entry.run_id
                )
            })?;
            args.jobs = vec![job_name.clone()];
        }
    }
    Ok(args)
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
        rerun_job: _,
        rerun_run_id: _,
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

async fn execute_with_config(config: ExecutorConfig) -> ExecutionOutcome {
    let engine_kind = config.engine;
    let run_result = match engine_kind {
        EngineKind::ContainerCli => {
            let executor = ContainerExecutor::new(config.clone())
                .with_context(|| "failed create container executor");
            match executor {
                Ok(executor) => executor.run().await,
                Err(err) => {
                    return ExecutionOutcome {
                        history_entry: None,
                        result: Err(err),
                    };
                }
            }
        }
        EngineKind::Docker => {
            let executor = DockerExecutor::new(config.clone())
                .with_context(|| "failed create docker executor");
            match executor {
                Ok(executor) => executor.run().await,
                Err(err) => {
                    return ExecutionOutcome {
                        history_entry: None,
                        result: Err(err),
                    };
                }
            }
        }
        EngineKind::Podman => {
            let executor = PodmanExecutor::new(config.clone())
                .with_context(|| "failed create podman executor");
            match executor {
                Ok(executor) => executor.run().await,
                Err(err) => {
                    return ExecutionOutcome {
                        history_entry: None,
                        result: Err(err),
                    };
                }
            }
        }
        EngineKind::Nerdctl => {
            let executor = NerdctlExecutor::new(config.clone())
                .with_context(|| "failed create nerdctl executor");
            match executor {
                Ok(executor) => executor.run().await,
                Err(err) => {
                    return ExecutionOutcome {
                        history_entry: None,
                        result: Err(err),
                    };
                }
            }
        }
        EngineKind::Orbstack => {
            let executor = OrbstackExecutor::new(config.clone())
                .with_context(|| "failed create orbstack executor");
            match executor {
                Ok(executor) => executor.run().await,
                Err(err) => {
                    return ExecutionOutcome {
                        history_entry: None,
                        result: Err(err),
                    };
                }
            }
        }
    };

    ExecutionOutcome {
        history_entry: run_result.history_entry,
        result: run_result.result.with_context(|| "failed to run pipeline"),
    }
}

fn run_capture_from_outcome(outcome: ExecutionOutcome) -> RunCapture {
    RunCapture {
        history_entry: outcome.history_entry,
        result: None,
        result_summary: None,
        error: outcome.result.err().map(|err| err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{prepare_run_args, run_capture_from_outcome};
    use crate::app::OpalApp;
    use crate::app::context::history_scope_root;
    use crate::executor::core::ExecutionOutcome;
    use crate::history::{HistoryEntry, HistoryJob, HistoryStatus, save};
    use crate::mcp::TEST_ENV_LOCK;
    use crate::runtime;
    use crate::{EngineChoice, RunArgs};
    use anyhow::anyhow;
    use std::env;
    use std::fs;
    use tempfile::tempdir;

    fn base_run_args() -> RunArgs {
        RunArgs {
            pipeline: None,
            workdir: None,
            base_image: None,
            env_includes: Vec::new(),
            max_parallel_jobs: 5,
            trace_scripts: false,
            engine: EngineChoice::Auto,
            no_tui: true,
            gitlab_base_url: None,
            gitlab_token: None,
            rerun_job: None,
            rerun_run_id: None,
            jobs: Vec::new(),
        }
    }

    #[test]
    fn prepare_run_args_sets_selected_job_from_latest_history() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-rerun-latest");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        let app = OpalApp::from_current_dir().expect("app");
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-1".to_string(),
                finished_at: "2026-03-31T12:00:00Z".to_string(),
                status: HistoryStatus::Failed,
                scope_root: Some(history_scope_root(&app.resolve_workdir(None))),
                ref_name: None,
                pipeline_file: None,
                jobs: vec![HistoryJob {
                    name: "rust-checks".to_string(),
                    stage: "test".to_string(),
                    status: HistoryStatus::Failed,
                    log_hash: "abc123".to_string(),
                    log_path: None,
                    artifact_dir: None,
                    artifacts: Vec::new(),
                    caches: Vec::new(),
                    container_name: None,
                    service_network: None,
                    service_containers: Vec::new(),
                    runtime_summary_path: None,
                    env_vars: Vec::new(),
                }],
            }],
        )
        .expect("save history");

        let mut args = base_run_args();
        args.rerun_job = Some("rust-checks".to_string());
        let prepared = prepare_run_args(&app, args).expect("prepare rerun args");

        assert_eq!(prepared.jobs, vec!["rust-checks"]);
        unsafe {
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn prepare_run_args_rejects_conflicting_job_selection() {
        let mut args = base_run_args();
        args.rerun_job = Some("rust-checks".to_string());
        args.jobs = vec!["build".to_string()];

        let app = OpalApp::from_current_dir().expect("app");
        let err = prepare_run_args(&app, args)
            .err()
            .expect("conflicting rerun args");
        assert!(
            err.to_string()
                .contains("--rerun-job cannot be combined with --job")
        );
    }

    #[test]
    fn run_capture_preserves_history_entry_for_failed_runs() {
        let entry = HistoryEntry {
            run_id: "run-failed".to_string(),
            finished_at: "now".to_string(),
            status: HistoryStatus::Failed,
            scope_root: Some("/tmp/repo".to_string()),
            ref_name: Some("main".to_string()),
            pipeline_file: Some(".gitlab-ci.yml".to_string()),
            jobs: vec![],
        };

        let capture = run_capture_from_outcome(ExecutionOutcome {
            history_entry: Some(entry.clone()),
            result: Err(anyhow!("boom")),
        });

        assert_eq!(
            capture
                .history_entry
                .as_ref()
                .map(|run| run.run_id.as_str()),
            Some("run-failed")
        );
        assert_eq!(capture.error.as_deref(), Some("boom"));
    }

    #[test]
    fn run_capture_uses_executor_reported_history_entry_directly() {
        let entry = HistoryEntry {
            run_id: "run-actual".to_string(),
            finished_at: "now".to_string(),
            status: HistoryStatus::Success,
            scope_root: Some("/tmp/repo".to_string()),
            ref_name: Some("main".to_string()),
            pipeline_file: Some(".gitlab-ci.yml".to_string()),
            jobs: vec![],
        };

        let capture = run_capture_from_outcome(ExecutionOutcome {
            history_entry: Some(entry.clone()),
            result: Ok(()),
        });

        assert_eq!(
            capture
                .history_entry
                .as_ref()
                .map(|run| run.run_id.as_str()),
            Some("run-actual")
        );
        assert!(capture.error.is_none());
    }
}
