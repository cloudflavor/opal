use super::OpalApp;
use super::context::{resolve_gitlab_remote, resolve_pipeline_path, rule_context_for_workdir};
use crate::PlanArgs;
use crate::compiler::compile_pipeline;
use crate::display::{self, DisplayFormatter};
use crate::execution_plan::{ExecutionPlan, build_execution_plan};
use crate::logging;
use crate::model::{DependencySourceSpec, PipelineSpec};
use crate::pipeline::{self, rules::RuleWhen};
use crate::runtime;
use crate::terminal;
use crate::ui;
use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::io::{self, IsTerminal};

pub(crate) struct RenderedPlan {
    pub content: String,
    pub json: bool,
}

pub(crate) fn execute(app: &OpalApp, args: PlanArgs) -> Result<()> {
    let use_pager = !args.no_pager;
    let rendered = render(app, args)?;
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

pub(crate) fn render(app: &OpalApp, args: PlanArgs) -> Result<RenderedPlan> {
    let workdir = app.resolve_workdir(args.workdir);
    let pipeline = resolve_pipeline_path(&workdir, args.pipeline);
    let gitlab = resolve_gitlab_remote(args.gitlab_base_url, args.gitlab_token);
    let pipeline_spec = PipelineSpec::from_path_with_gitlab(&pipeline, gitlab.as_ref())
        .with_context(|| format!("failed to load pipeline {}", pipeline.display()))?;
    let ctx = rule_context_for_workdir(&workdir);
    ctx.ensure_valid_tag_context()?;
    if !pipeline::rules::filters_allow(&pipeline_spec.filters, &ctx) {
        return Ok(RenderedPlan {
            content: "pipeline skipped: top-level only/except filters exclude this ref".to_string(),
            json: false,
        });
    }
    if let Some(workflow) = &pipeline_spec.workflow
        && !pipeline::rules::evaluate_workflow(&workflow.rules, &ctx)?
    {
        return Ok(RenderedPlan {
            content: "pipeline skipped: workflow rules excluded this run".to_string(),
            json: false,
        });
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
