mod coordinator;
mod manual;
mod resource_groups;
mod retry;

use self::coordinator::ExecutionCoordinator;
use super::{core::ExecutorCore, job_runner};
use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::model::ArtifactSourceOutcome;
use crate::pipeline::{JobEvent, JobStatus, JobSummary};
use crate::ui::{UiBridge, UiCommand};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::{sync::mpsc, task};

fn interruptible_running_jobs(plan: &ExecutionPlan, running: &HashSet<String>) -> Vec<String> {
    let mut names = running
        .iter()
        .filter(|name| {
            plan.nodes
                .get(*name)
                .map(|planned| planned.instance.interruptible)
                .unwrap_or(false)
        })
        .cloned()
        .collect::<Vec<_>>();
    names.sort_by_key(|name| plan.order_index.get(name).copied().unwrap_or(usize::MAX));
    names
}

pub(crate) async fn execute_plan(
    exec: &ExecutorCore,
    plan: Arc<ExecutionPlan>,
    ui: Option<Arc<UiBridge>>,
    commands: Option<&mut mpsc::UnboundedReceiver<UiCommand>>,
) -> (Vec<JobSummary>, Result<()>) {
    ExecutionCoordinator::new(exec, plan, ui, commands)
        .run()
        .await
}

pub(crate) async fn handle_restart_commands(
    exec: &ExecutorCore,
    plan: Arc<ExecutionPlan>,
    ui: Option<Arc<UiBridge>>,
    commands: &mut mpsc::UnboundedReceiver<UiCommand>,
    summaries: &mut Vec<JobSummary>,
) -> Result<()> {
    while let Some(command) = commands.recv().await {
        match command {
            UiCommand::RestartJob { name } => {
                let Some(planned) = plan.nodes.get(&name).cloned() else {
                    continue;
                };

                if let Some(ui_ref) = ui.as_deref() {
                    ui_ref.job_restarted(&name);
                }

                let run_info = match exec.log_job_start(&planned, ui.as_deref()) {
                    Ok(info) => info,
                    Err(err) => {
                        summaries.push(planned_summary(
                            exec,
                            &planned,
                            0.0,
                            JobStatus::Failed(err.to_string()),
                            Some(planned.log_path.clone()),
                            false,
                        ));
                        return Err(err);
                    }
                };
                let restart_exec = exec.clone();
                let ui_clone = ui.clone();
                let run_info_clone = run_info.clone();
                let job_plan = plan.clone();
                let runtime_handle = tokio::runtime::Handle::current();
                let event = task::spawn_blocking(move || {
                    job_runner::run_planned_job(
                        &restart_exec,
                        &runtime_handle,
                        job_plan,
                        planned,
                        run_info_clone,
                        ui_clone,
                    )
                })
                .await
                .context("job restart task failed")?;
                update_summaries_from_event(exec, plan.as_ref(), event, summaries);
            }
            UiCommand::AnalyzeJob { name, source_name } => {
                spawn_analysis(
                    Arc::new(exec.clone()),
                    plan.clone(),
                    ui.clone(),
                    name,
                    source_name,
                );
            }
            UiCommand::PreviewAiPrompt { name, source_name } => {
                spawn_prompt_preview(
                    Arc::new(exec.clone()),
                    plan.clone(),
                    ui.clone(),
                    name,
                    source_name,
                );
            }
            UiCommand::StartManual { .. } => {}
            UiCommand::CancelJob { .. } => {}
            UiCommand::AbortPipeline => break,
        }
    }
    Ok(())
}

fn spawn_analysis(
    exec: Arc<ExecutorCore>,
    plan: Arc<ExecutionPlan>,
    ui: Option<Arc<UiBridge>>,
    name: String,
    source_name: String,
) {
    tokio::spawn(async move {
        exec.analyze_job_with_default_provider(&plan, &name, &source_name, ui.as_deref())
            .await;
    });
}

fn spawn_prompt_preview(
    exec: Arc<ExecutorCore>,
    plan: Arc<ExecutionPlan>,
    ui: Option<Arc<UiBridge>>,
    name: String,
    source_name: String,
) {
    tokio::task::spawn_blocking(move || {
        if let Some(ui) = ui.as_deref()
            && let Ok(prompt) = exec.render_ai_prompt(&plan, &name, &source_name)
        {
            ui.ai_prompt_ready(&name, prompt);
        }
    });
}

pub(super) fn planned_summary(
    exec: &ExecutorCore,
    planned: &ExecutableJob,
    duration: f32,
    status: JobStatus,
    log_path: Option<PathBuf>,
    allow_failure: bool,
) -> JobSummary {
    JobSummary {
        name: planned.instance.job.name.clone(),
        stage_name: planned.instance.stage_name.clone(),
        duration,
        status,
        log_path,
        log_hash: planned.log_hash.clone(),
        allow_failure,
        environment: exec.expanded_environment(&planned.instance.job),
    }
}

fn update_summaries_from_event(
    exec: &ExecutorCore,
    plan: &ExecutionPlan,
    event: JobEvent,
    summaries: &mut Vec<JobSummary>,
) {
    let JobEvent {
        name,
        stage_name,
        duration,
        log_path,
        log_hash,
        result,
        failure_kind: _,
        exit_code: _,
        cancelled,
    } = event;

    let status = match result {
        Ok(_) => JobStatus::Success,
        Err(err) => {
            if cancelled {
                JobStatus::Skipped("aborted by user".to_string())
            } else {
                JobStatus::Failed(err.to_string())
            }
        }
    };
    let outcome = match &status {
        JobStatus::Success => ArtifactSourceOutcome::Success,
        JobStatus::Failed(_) => ArtifactSourceOutcome::Failed,
        JobStatus::Skipped(_) => ArtifactSourceOutcome::Skipped,
    };
    exec.record_completed_job(&name, outcome);

    let summary = if let Some(planned) = plan.nodes.get(&name) {
        planned_summary(
            exec,
            planned,
            duration,
            status,
            log_path,
            planned.instance.rule.allow_failure,
        )
    } else {
        JobSummary {
            name: name.clone(),
            stage_name,
            duration,
            status,
            log_path,
            log_hash,
            allow_failure: false,
            environment: None,
        }
    };

    summaries.retain(|entry| entry.name != name);
    summaries.push(summary);
}

#[cfg(test)]
mod tests {
    use super::{interruptible_running_jobs, retry::retry_allowed};
    use crate::compiler::JobInstance;
    use crate::execution_plan::{ExecutableJob, ExecutionPlan};
    use crate::model::{ArtifactSpec, JobSpec, RetryPolicySpec};
    use crate::pipeline::{JobFailureKind, RuleEvaluation};
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    #[test]
    fn retry_allowed_defaults_to_true_without_conditions() {
        assert!(retry_allowed(
            &[],
            &[],
            Some(JobFailureKind::ScriptFailure),
            Some(1)
        ));
    }

    #[test]
    fn retry_allowed_matches_script_failure_condition() {
        assert!(retry_allowed(
            &["script_failure".into()],
            &[],
            Some(JobFailureKind::ScriptFailure),
            Some(1)
        ));
        assert!(!retry_allowed(
            &["runner_system_failure".into()],
            &[],
            Some(JobFailureKind::ScriptFailure),
            Some(1)
        ));
    }

    #[test]
    fn retry_allowed_treats_job_timeout_as_stuck_or_timeout_failure() {
        assert!(retry_allowed(
            &["stuck_or_timeout_failure".into()],
            &[],
            Some(JobFailureKind::JobExecutionTimeout),
            None
        ));
    }

    #[test]
    fn retry_allowed_matches_api_failure_condition() {
        assert!(retry_allowed(
            &["api_failure".into()],
            &[],
            Some(JobFailureKind::ApiFailure),
            None
        ));
        assert!(!retry_allowed(
            &["api_failure".into()],
            &[],
            Some(JobFailureKind::UnknownFailure),
            None
        ));
    }

    #[test]
    fn retry_allowed_matches_unmet_prerequisites_condition() {
        assert!(retry_allowed(
            &["unmet_prerequisites".into()],
            &[],
            Some(JobFailureKind::UnmetPrerequisites),
            None
        ));
    }

    #[test]
    fn retry_allowed_matches_exit_code_condition() {
        assert!(retry_allowed(
            &[],
            &[137],
            Some(JobFailureKind::ScriptFailure),
            Some(137)
        ));
        assert!(!retry_allowed(
            &[],
            &[137],
            Some(JobFailureKind::ScriptFailure),
            Some(1)
        ));
    }

    #[test]
    fn retry_allowed_matches_when_or_exit_code() {
        assert!(retry_allowed(
            &["runner_system_failure".into()],
            &[137],
            Some(JobFailureKind::ScriptFailure),
            Some(137)
        ));
    }

    #[test]
    fn interruptible_running_jobs_selects_only_interruptible_nodes() {
        let plan = ExecutionPlan {
            ordered: vec!["build".into(), "deploy".into()],
            nodes: HashMap::from([
                ("build".into(), executable_job("build", true, "build", 0)),
                (
                    "deploy".into(),
                    executable_job("deploy", false, "deploy", 1),
                ),
            ]),
            dependents: HashMap::new(),
            order_index: HashMap::from([("build".into(), 0), ("deploy".into(), 1)]),
            variants: HashMap::new(),
        };
        let running = HashSet::from(["build".to_string(), "deploy".to_string()]);

        assert_eq!(interruptible_running_jobs(&plan, &running), vec!["build"]);
    }

    #[test]
    fn interruptible_running_jobs_respects_plan_order() {
        let plan = ExecutionPlan {
            ordered: vec!["test".into(), "build".into()],
            nodes: HashMap::from([
                ("build".into(), executable_job("build", true, "build", 1)),
                ("test".into(), executable_job("test", true, "test", 0)),
            ]),
            dependents: HashMap::new(),
            order_index: HashMap::from([("test".into(), 0), ("build".into(), 1)]),
            variants: HashMap::new(),
        };
        let running = HashSet::from(["build".to_string(), "test".to_string()]);

        assert_eq!(
            interruptible_running_jobs(&plan, &running),
            vec!["test", "build"]
        );
    }

    fn executable_job(name: &str, interruptible: bool, stage: &str, order: usize) -> ExecutableJob {
        let mut job = job(name);
        job.interruptible = interruptible;
        ExecutableJob {
            instance: JobInstance {
                job,
                stage_name: stage.into(),
                dependencies: Vec::new(),
                rule: RuleEvaluation::default(),
                timeout: None,
                retry: RetryPolicySpec::default(),
                interruptible,
                resource_group: None,
            },
            log_path: PathBuf::from(format!("/tmp/{name}-{order}.log")),
            log_hash: format!("hash-{name}-{order}"),
        }
    }

    fn job(name: &str) -> JobSpec {
        JobSpec {
            name: name.into(),
            stage: "build".into(),
            commands: vec!["true".into()],
            needs: Vec::new(),
            explicit_needs: false,
            dependencies: Vec::new(),
            before_script: None,
            after_script: None,
            inherit_default_before_script: true,
            inherit_default_after_script: true,
            inherit_default_image: true,
            inherit_default_cache: true,
            inherit_default_services: true,
            inherit_default_timeout: true,
            inherit_default_retry: true,
            inherit_default_interruptible: true,
            when: None,
            rules: Vec::new(),
            only: Vec::new(),
            except: Vec::new(),
            artifacts: ArtifactSpec::default(),
            cache: Vec::new(),
            image: None,
            variables: HashMap::new(),
            services: Vec::new(),
            timeout: None,
            retry: RetryPolicySpec::default(),
            interruptible: false,
            resource_group: None,
            parallel: None,
            tags: Vec::new(),
            environment: None,
        }
    }
}
