use super::OpalApp;
use super::context::{resolve_gitlab_remote, resolve_pipeline_path, rule_context_for_workdir};
use crate::PlanArgs;
use crate::compiler::compile_pipeline;
use crate::display::{self, DisplayFormatter};
use crate::execution_plan::{ExecutionPlan, build_execution_plan};
use crate::logging;
use crate::model::{DependencySourceSpec, PipelineSpec};
use crate::pipeline::{
    self,
    rules::{RuleWhen, apply_when_config, evaluate_rules, filters_allow},
};
use crate::runtime;
use crate::terminal;
use crate::ui;
use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::PathBuf;

pub(crate) struct RenderedPlan {
    pub content: String,
    pub json: bool,
}

pub(crate) struct ExplainedPlan {
    pub summary: String,
    pub details: PlanExplainJson,
}

#[derive(Serialize)]
pub(crate) struct PlanExplainJson {
    pipeline: String,
    job: ExplainedJobJson,
}

#[derive(Serialize)]
struct ExplainedJobJson {
    selector: String,
    resolved_name: Option<String>,
    status: String,
    reason: String,
    stage: Option<String>,
    when: Option<String>,
    manual_reason: Option<String>,
    start_in: Option<String>,
    dependencies: Vec<String>,
    dependents: Vec<String>,
    selectors: Vec<String>,
    selected: bool,
    selected_directly: bool,
    variants: Vec<String>,
}

enum PreparedPlan {
    Skipped {
        pipeline: PathBuf,
        reason: String,
    },
    Ready {
        pipeline: PathBuf,
        pipeline_spec: Box<PipelineSpec>,
        ctx: crate::compiler::CompileContext,
        full_plan: Box<ExecutionPlan>,
        selected_plan: Box<ExecutionPlan>,
        selectors: Vec<String>,
    },
}

pub(crate) async fn execute(app: &OpalApp, args: PlanArgs) -> Result<()> {
    let use_pager = !args.no_pager;
    let rendered = render(app, args).await?;
    if rendered.json {
        println!("{}", rendered.content);
        return Ok(());
    }
    if use_pager && io::stdout().is_terminal() {
        ui::page_text_with_pager(&rendered.content)?;
    } else {
        println!("{}", rendered.content);
    }
    Ok(())
}

pub(crate) async fn render(app: &OpalApp, args: PlanArgs) -> Result<RenderedPlan> {
    let plan = prepare_plan(app, &args).await?;
    let plan = match plan {
        PreparedPlan::Skipped { reason, .. } => {
            return Ok(RenderedPlan {
                content: reason,
                json: false,
            });
        }
        PreparedPlan::Ready { selected_plan, .. } => *selected_plan,
    };
    if args.json {
        let payload = plan_json(&plan);
        return Ok(RenderedPlan {
            content: serde_json::to_string_pretty(&payload)?,
            json: true,
        });
    }
    let display = DisplayFormatter::new(terminal::should_use_color());
    let text = display::collect_pipeline_plan(&display, &plan).join("\n");
    Ok(RenderedPlan {
        content: text,
        json: false,
    })
}

pub(crate) async fn explain(
    app: &OpalApp,
    args: PlanArgs,
    job_selector: &str,
) -> Result<ExplainedPlan> {
    let selector = job_selector.to_string();
    let prepared = prepare_plan(app, &args).await?;

    match prepared {
        PreparedPlan::Skipped { pipeline, reason } => Ok(ExplainedPlan {
            summary: format!(
                "Job '{}' is skipped because {}",
                job_selector,
                reason.trim_start_matches("pipeline skipped: ")
            ),
            details: PlanExplainJson {
                pipeline: pipeline.display().to_string(),
                job: ExplainedJobJson {
                    selector,
                    resolved_name: None,
                    status: "skipped".to_string(),
                    reason,
                    stage: None,
                    when: None,
                    manual_reason: None,
                    start_in: None,
                    dependencies: Vec::new(),
                    dependents: Vec::new(),
                    selectors: args.jobs,
                    selected: false,
                    selected_directly: false,
                    variants: Vec::new(),
                },
            },
        }),
        PreparedPlan::Ready {
            pipeline,
            pipeline_spec,
            ctx,
            full_plan,
            selected_plan,
            selectors,
        } => explain_ready_plan(
            pipeline,
            *pipeline_spec,
            &ctx,
            &full_plan,
            &selected_plan,
            selectors,
            selector,
        ),
    }
}

#[derive(Serialize)]
struct PlanJson {
    jobs: Vec<PlanJobJson>,
}

#[derive(Serialize)]
struct PlanJobJson {
    name: String,
    stage: String,
    when: Option<String>,
    allow_failure: bool,
    start_in: Option<String>,
    dependencies: Vec<String>,
    needs: Vec<PlanNeedJson>,
    artifacts: Vec<String>,
    caches: Vec<String>,
    image: Option<String>,
    services: Vec<String>,
    tags: Vec<String>,
    environment: Option<String>,
    resource_group: Option<String>,
    interruptible: bool,
    retry_max: u32,
}

#[derive(Serialize)]
struct PlanNeedJson {
    job: String,
    artifacts: bool,
    optional: bool,
    source: String,
}

fn plan_json(plan: &ExecutionPlan) -> PlanJson {
    let jobs = plan
        .ordered
        .iter()
        .filter_map(|name| plan.nodes.get(name))
        .map(|planned| {
            let job = &planned.instance.job;
            let when = Some(rule_when_label(planned.instance.rule.when).to_string())
                .or_else(|| job.when.clone());
            let start_in = planned
                .instance
                .rule
                .start_in
                .map(|d| humantime::format_duration(d).to_string());
            let needs = job
                .needs
                .iter()
                .map(|need| PlanNeedJson {
                    job: need.job.clone(),
                    artifacts: need.needs_artifacts,
                    optional: need.optional,
                    source: match &need.source {
                        DependencySourceSpec::Local => "local".to_string(),
                        DependencySourceSpec::External(ext) => {
                            format!("external:{}@{}", ext.project, ext.reference)
                        }
                    },
                })
                .collect();
            PlanJobJson {
                name: job.name.clone(),
                stage: planned.instance.stage_name.clone(),
                when,
                allow_failure: planned.instance.rule.allow_failure,
                start_in,
                dependencies: planned.instance.dependencies.clone(),
                needs,
                artifacts: job
                    .artifacts
                    .paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
                caches: job.cache.iter().map(|cache| cache.key.describe()).collect(),
                image: job.image.as_ref().map(|image| image.name.clone()),
                services: job.services.iter().map(|svc| svc.image.clone()).collect(),
                tags: job.tags.clone(),
                environment: job.environment.as_ref().map(|env| env.name.clone()),
                resource_group: planned.instance.resource_group.clone(),
                interruptible: planned.instance.interruptible,
                retry_max: planned.instance.retry.max,
            }
        })
        .collect();
    PlanJson { jobs }
}

async fn prepare_plan(app: &OpalApp, args: &PlanArgs) -> Result<PreparedPlan> {
    let workdir = app.resolve_workdir(args.workdir.clone());
    let pipeline = resolve_pipeline_path(&workdir, args.pipeline.clone());
    let gitlab = resolve_gitlab_remote(args.gitlab_base_url.clone(), args.gitlab_token.clone());
    let pipeline_spec = PipelineSpec::from_path_with_gitlab_async(&pipeline, gitlab.as_ref())
        .await
        .with_context(|| format!("failed to load pipeline {}", pipeline.display()))?;
    let ctx = rule_context_for_workdir(&workdir);
    ctx.ensure_valid_tag_context()?;
    if !pipeline::rules::filters_allow(&pipeline_spec.filters, &ctx) {
        return Ok(PreparedPlan::Skipped {
            pipeline,
            reason: "pipeline skipped: top-level only/except filters exclude this ref".to_string(),
        });
    }
    if let Some(workflow) = &pipeline_spec.workflow
        && !pipeline::rules::evaluate_workflow(&workflow.rules, &ctx)?
    {
        return Ok(PreparedPlan::Skipped {
            pipeline,
            reason: "pipeline skipped: workflow rules excluded this run".to_string(),
        });
    }

    let logs_dir = runtime::runs_root().join("plan/logs");
    fs::create_dir_all(&logs_dir)
        .with_context(|| format!("failed to create plan log dir {}", logs_dir.display()))?;
    let compiled = compile_pipeline(&pipeline_spec, Some(&ctx))?;
    let full_plan = build_execution_plan(compiled, |job| {
        logging::job_log_info(&logs_dir, "plan-preview", job)
    })?;
    let selected_plan = if args.jobs.is_empty() {
        full_plan.clone()
    } else {
        full_plan.select_jobs(&args.jobs)?
    };

    Ok(PreparedPlan::Ready {
        pipeline,
        pipeline_spec: Box::new(pipeline_spec),
        ctx,
        full_plan: Box::new(full_plan),
        selected_plan: Box::new(selected_plan),
        selectors: args.jobs.clone(),
    })
}

fn explain_ready_plan(
    pipeline: PathBuf,
    pipeline_spec: PipelineSpec,
    ctx: &crate::compiler::CompileContext,
    full_plan: &ExecutionPlan,
    selected_plan: &ExecutionPlan,
    selectors: Vec<String>,
    selector: String,
) -> Result<ExplainedPlan> {
    if let Some(planned) = full_plan.nodes.get(&selector) {
        let selected = selected_plan.nodes.contains_key(&selector);
        let selected_directly = selector_matches_job(full_plan, &selectors, &selector);
        let (status, reason) = explain_selected_job_reason(
            planned.instance.job.name.as_str(),
            &selectors,
            selected,
            selected_directly,
        );
        let dependents = full_plan
            .dependents
            .get(&selector)
            .cloned()
            .unwrap_or_default();
        return Ok(ExplainedPlan {
            summary: format!("Job '{}' is {}", selector, status),
            details: PlanExplainJson {
                pipeline: pipeline.display().to_string(),
                job: ExplainedJobJson {
                    selector,
                    resolved_name: Some(planned.instance.job.name.clone()),
                    status: status.to_string(),
                    reason,
                    stage: Some(planned.instance.stage_name.clone()),
                    when: Some(rule_when_label(planned.instance.rule.when).to_string()),
                    manual_reason: planned.instance.rule.manual_reason.clone(),
                    start_in: planned
                        .instance
                        .rule
                        .start_in
                        .map(|d| humantime::format_duration(d).to_string()),
                    dependencies: planned.instance.dependencies.clone(),
                    dependents,
                    selectors,
                    selected,
                    selected_directly,
                    variants: Vec::new(),
                },
            },
        });
    }

    if let Some(variants) = full_plan.variants.get(&selector)
        && !variants.is_empty()
    {
        let selected_variants = variants
            .iter()
            .filter(|variant| selected_plan.nodes.contains_key(&variant.name))
            .map(|variant| variant.name.clone())
            .collect::<Vec<_>>();
        let selected = !selected_variants.is_empty();
        let selected_directly = selector_matches_job(full_plan, &selectors, &selector);
        let (status, reason) = explain_parallel_job_reason(&selector, &selectors, selected);
        let stage = pipeline_spec
            .jobs
            .get(&selector)
            .map(|job| job.stage.clone());
        return Ok(ExplainedPlan {
            summary: format!("Job '{}' is {}", selector, status),
            details: PlanExplainJson {
                pipeline: pipeline.display().to_string(),
                job: ExplainedJobJson {
                    selector: selector.clone(),
                    resolved_name: Some(selector),
                    status: status.to_string(),
                    reason,
                    stage,
                    when: None,
                    manual_reason: None,
                    start_in: None,
                    dependencies: Vec::new(),
                    dependents: Vec::new(),
                    selectors,
                    selected,
                    selected_directly,
                    variants: variants
                        .iter()
                        .map(|variant| variant.name.clone())
                        .collect(),
                },
            },
        });
    }

    if let Some(job) = pipeline_spec.jobs.get(&selector) {
        let mut evaluation = evaluate_rules(job, ctx)?;
        if job.rules.is_empty() {
            apply_when_config(
                &mut evaluation,
                job.when.as_deref(),
                None,
                Some("manual job"),
            );
        }

        let reason = if !filters_allow(job, ctx) {
            "job is skipped because only/except filters exclude it for the current ref".to_string()
        } else if !evaluation.included {
            match evaluation.when {
                RuleWhen::Never => {
                    "job is skipped because its rules or when configuration resolved to never"
                        .to_string()
                }
                _ => "job is skipped because no rule matched for the current context".to_string(),
            }
        } else {
            "job is defined in the pipeline but is not present in the execution plan".to_string()
        };

        return Ok(ExplainedPlan {
            summary: format!("Job '{}' is skipped", selector),
            details: PlanExplainJson {
                pipeline: pipeline.display().to_string(),
                job: ExplainedJobJson {
                    selector,
                    resolved_name: Some(job.name.clone()),
                    status: "skipped".to_string(),
                    reason,
                    stage: Some(job.stage.clone()),
                    when: Some(rule_when_label(evaluation.when).to_string()),
                    manual_reason: evaluation.manual_reason,
                    start_in: evaluation
                        .start_in
                        .map(|d| humantime::format_duration(d).to_string()),
                    dependencies: Vec::new(),
                    dependents: Vec::new(),
                    selectors,
                    selected: false,
                    selected_directly: false,
                    variants: Vec::new(),
                },
            },
        });
    }

    Ok(ExplainedPlan {
        summary: format!("Job '{}' was not found in the pipeline", selector),
        details: PlanExplainJson {
            pipeline: pipeline.display().to_string(),
            job: ExplainedJobJson {
                selector,
                resolved_name: None,
                status: "unknown".to_string(),
                reason: "job selector did not match a known job in the pipeline".to_string(),
                stage: None,
                when: None,
                manual_reason: None,
                start_in: None,
                dependencies: Vec::new(),
                dependents: Vec::new(),
                selectors,
                selected: false,
                selected_directly: false,
                variants: Vec::new(),
            },
        },
    })
}

fn selector_matches_job(plan: &ExecutionPlan, selectors: &[String], job_name: &str) -> bool {
    selectors.iter().any(|selector| {
        selector == job_name
            || plan
                .variants
                .get(selector)
                .is_some_and(|variants| variants.iter().any(|variant| variant.name == job_name))
    })
}

fn explain_selected_job_reason(
    job_name: &str,
    selectors: &[String],
    selected: bool,
    selected_directly: bool,
) -> (&'static str, String) {
    if selected {
        if selectors.is_empty() {
            (
                "included",
                format!("job '{job_name}' is included in the full execution plan"),
            )
        } else if selected_directly {
            (
                "included",
                format!("job '{job_name}' matched the requested selector set"),
            )
        } else {
            (
                "included",
                format!(
                    "job '{job_name}' is included because it is required by the requested upstream closure"
                ),
            )
        }
    } else {
        (
            "blocked",
            format!(
                "job '{job_name}' exists in the full execution plan but is excluded by the requested selector set"
            ),
        )
    }
}

fn explain_parallel_job_reason(
    job_name: &str,
    selectors: &[String],
    selected: bool,
) -> (&'static str, String) {
    if selected {
        if selectors.is_empty() {
            (
                "included",
                format!(
                    "job '{job_name}' expands into parallel variants that are present in the execution plan"
                ),
            )
        } else {
            (
                "included",
                format!(
                    "job '{job_name}' expands into parallel variants that are included in the requested selector set"
                ),
            )
        }
    } else {
        (
            "blocked",
            format!(
                "job '{job_name}' expands into parallel variants, but none are included by the requested selector set"
            ),
        )
    }
}

fn rule_when_label(value: RuleWhen) -> &'static str {
    match value {
        RuleWhen::OnSuccess => "on_success",
        RuleWhen::Manual => "manual",
        RuleWhen::Delayed => "delayed",
        RuleWhen::Never => "never",
        RuleWhen::Always => "always",
        RuleWhen::OnFailure => "on_failure",
    }
}
