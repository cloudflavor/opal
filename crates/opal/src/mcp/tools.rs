use crate::app::OpalApp;
use crate::app::context::{resolve_engine_choice, resolve_pipeline_path};
use crate::app::plan::{explain as explain_plan, render as render_plan};
use crate::app::run::{RunCapture, execute_and_capture_with_progress};
use crate::app::view::{
    find_history_entry_for_workdir, find_job, latest_history_entry_for_workdir,
    load_history_for_workdir, read_job_log, read_runtime_summary,
};
use crate::config::OpalConfig;
use crate::executor::core::{ExecutionProgressCallback, ExecutionProgressEvent, ProgressJobStatus};
use crate::history::{HistoryEntry, HistoryStatus};
use crate::{EngineChoice, EngineKind, PlanArgs, RunArgs, ViewArgs};
use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use time::OffsetDateTime;

pub(crate) fn list_tools() -> Value {
    json!({
        "tools": [
            {
                "name": "opal_plan",
                "title": "Render an Opal pipeline plan",
                "description": "Evaluates a local .gitlab-ci.yml and returns the formatted plan or JSON plan.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workdir": { "type": "string" },
                        "pipeline": { "type": "string" },
                        "gitlab_base_url": { "type": "string" },
                        "gitlab_token": { "type": "string" },
                        "jobs": { "type": "array", "items": { "type": "string" } },
                        "json": { "type": "boolean" }
                    }
                }
            },
            {
                "name": "opal_run",
                "title": "Run an Opal pipeline",
                "description": "Starts a local pipeline run in the background and returns an operation handle for status polling.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workdir": { "type": "string" },
                        "pipeline": { "type": "string" },
                        "base_image": { "type": "string" },
                        "env_includes": { "type": "array", "items": { "type": "string" } },
                        "max_parallel_jobs": { "type": "integer", "minimum": 1 },
                        "trace_scripts": { "type": "boolean" },
                        "engine": { "type": "string", "enum": EngineChoice::VARIANTS },
                        "gitlab_base_url": { "type": "string" },
                        "gitlab_token": { "type": "string" },
                        "jobs": { "type": "array", "items": { "type": "string" } }
                    }
                }
            },
            {
                "name": "opal_run_status",
                "title": "Inspect a background Opal operation",
                "description": "Returns the current or final status for a background Opal MCP operation, including run, rerun, and log-search requests.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "operation_id": { "type": "string" }
                    },
                    "required": ["operation_id"]
                }
            },
            {
                "name": "opal_operations_list",
                "title": "List background Opal operations",
                "description": "Lists active and recent background Opal MCP operations with optional filters.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "status": { "type": "string", "enum": ["running", "succeeded", "failed"] },
                        "active_only": { "type": "boolean" },
                        "limit": { "type": "integer", "minimum": 1 }
                    }
                }
            },
            {
                "name": "opal_view",
                "title": "Inspect Opal history and logs",
                "description": "Returns the latest or selected recorded Opal run, with optional job log and runtime summary.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workdir": { "type": "string" },
                        "run_id": { "type": "string" },
                        "job": { "type": "string" },
                        "include_log": { "type": "boolean" },
                        "include_runtime_summary": { "type": "boolean" }
                    }
                }
            },
            {
                "name": "opal_failed_jobs",
                "title": "List failed jobs for a recorded Opal run",
                "description": "Returns the failed jobs for the latest or a selected recorded Opal run.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workdir": { "type": "string" },
                        "run_id": { "type": "string" }
                    }
                }
            },
            {
                "name": "opal_history_list",
                "title": "List recorded Opal runs",
                "description": "Returns recorded Opal runs with optional status, job-name, date-range, branch, and pipeline-file filters.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "enum": ["success", "failed", "skipped", "running"]
                        },
                        "workdir": { "type": "string" },
                        "job": { "type": "string" },
                        "branch": { "type": "string" },
                        "pipeline_file": { "type": "string" },
                        "since": { "type": "string" },
                        "until": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1 }
                    }
                }
            },
            {
                "name": "opal_run_diff",
                "title": "Compare recorded Opal runs",
                "description": "Compares two recorded Opal runs and summarizes overall and per-job changes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workdir": { "type": "string" },
                        "run_id": { "type": "string" },
                        "base_run_id": { "type": "string" }
                    }
                }
            },
            {
                "name": "opal_logs_search",
                "title": "Search recorded Opal job logs",
                "description": "Starts a background search of recorded Opal job logs for recurring failures or exact strings.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workdir": { "type": "string" },
                        "query": { "type": "string" },
                        "run_id": { "type": "string" },
                        "job": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1 },
                        "case_sensitive": { "type": "boolean" }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "opal_job_rerun",
                "title": "Rerun a recorded job name",
                "description": "Reruns a job name from the latest or a selected recorded run against the current checkout, letting Opal include upstream closure automatically.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "run_id": { "type": "string" },
                        "job": { "type": "string" },
                        "workdir": { "type": "string" },
                        "pipeline": { "type": "string" },
                        "base_image": { "type": "string" },
                        "env_includes": { "type": "array", "items": { "type": "string" } },
                        "max_parallel_jobs": { "type": "integer", "minimum": 1 },
                        "trace_scripts": { "type": "boolean" },
                        "engine": { "type": "string", "enum": EngineChoice::VARIANTS },
                        "gitlab_base_url": { "type": "string" },
                        "gitlab_token": { "type": "string" }
                    },
                    "required": ["job"]
                }
            },
            {
                "name": "opal_plan_explain",
                "title": "Explain a job's plan status",
                "description": "Explains why a job is included, skipped, or blocked in the evaluated plan.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "job": { "type": "string" },
                        "workdir": { "type": "string" },
                        "pipeline": { "type": "string" },
                        "gitlab_base_url": { "type": "string" },
                        "gitlab_token": { "type": "string" },
                        "jobs": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["job"]
                }
            },
            {
                "name": "opal_engine_status",
                "title": "Report local Opal engine availability",
                "description": "Reports Opal's configured default engine, auto resolution, and per-engine local availability.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workdir": { "type": "string" }
                    }
                }
            }
        ]
    })
}

pub(crate) async fn call_tool(app: &OpalApp, params: Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .context("missing tool name")?;
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

    match name {
        "opal_plan" => {
            let rendered = render_plan(app, plan_args_from_value(arguments)?).await?;
            Ok(tool_result(
                rendered.content,
                json!({
                    "json": rendered.json
                }),
                false,
            ))
        }
        "opal_run" => Ok(run_tool(app, arguments).await),
        "opal_run_status" => Ok(run_status_tool(arguments).await?),
        "opal_operations_list" => Ok(operations_list_tool(arguments).await?),
        "opal_view" => Ok(view_tool(app, arguments).await),
        "opal_failed_jobs" => Ok(failed_jobs_tool(app, arguments).await?),
        "opal_history_list" => Ok(history_list_tool(app, arguments).await?),
        "opal_run_diff" => Ok(run_diff_tool(app, arguments).await?),
        "opal_logs_search" => Ok(logs_search_tool(app, arguments).await),
        "opal_job_rerun" => Ok(job_rerun_tool(app, arguments).await),
        "opal_plan_explain" => Ok(plan_explain_tool(app, arguments).await?),
        "opal_engine_status" => Ok(engine_status_tool(app, arguments).await?),
        other => Ok(error_tool_result(
            format!("unknown tool: {other}"),
            Value::Null,
        )),
    }
}

async fn run_tool(app: &OpalApp, arguments: Value) -> Value {
    let Ok(args) = run_args_from_value(arguments) else {
        return error_tool_result("invalid run arguments".to_string(), Value::Null);
    };
    let requested_jobs = args.jobs.clone();
    let resolved_workdir = app.resolve_workdir(args.workdir.clone());
    let resolved_pipeline = resolve_pipeline_path(&resolved_workdir, args.pipeline.clone());
    let resolved_engine = resolved_engine_label(&resolved_workdir, args.engine).await;
    let operation_request = RunOperationRequest {
        tool: "opal_run",
        dedupe_key: Some(operation_dedupe_key(
            "opal_run",
            [
                ("workdir", Some(resolved_workdir.display().to_string())),
                ("pipeline", Some(resolved_pipeline.display().to_string())),
                ("jobs", Some(normalized_jobs_key(&requested_jobs))),
                ("rerun_job", None),
                ("rerun_run_id", None),
            ],
        )),
        workdir: Some(resolved_workdir.display().to_string()),
        pipeline: Some(resolved_pipeline.display().to_string()),
        resolved_engine,
        run_id: None,
        requested_jobs,
        requested_job: None,
        source_run_id: None,
    };
    let operations = run_operations();
    let start = operations.start_with_operation_id(operation_request, {
        let app = app.clone();
        let operations = operations.clone();
        move |operation_id| {
            let progress = operation_progress_callback(operations, operation_id);
            async move { execute_and_capture_with_progress(&app, args, Some(progress)).await }
        }
    });
    let operation = start.operation;
    tool_result(
        if start.reused_existing {
            format!(
                "Reusing background Opal run operation {}. Poll opal_run_status with this operation_id.",
                operation.operation_id
            )
        } else {
            format!(
                "Started background Opal run operation {}. Poll opal_run_status with this operation_id.",
                operation.operation_id
            )
        },
        json!({
            "operation": operation,
            "deduped": start.reused_existing,
        }),
        false,
    )
}

async fn run_status_tool(arguments: Value) -> Result<Value> {
    let operation_id = arguments
        .get("operation_id")
        .and_then(Value::as_str)
        .context("missing operation_id")?;
    let operation = run_operations()
        .status_view(operation_id)
        .await
        .with_context(|| format!("background operation '{operation_id}' not found"))?;
    let RunOperationStatusView {
        operation,
        age_seconds,
        last_update_age_seconds,
        progress_percent,
        is_stale,
        current_jobs,
    } = operation;

    let operation_value = operation_status_value(
        &operation,
        age_seconds,
        last_update_age_seconds,
        progress_percent,
        is_stale,
    )?;
    let mut structured = json!({ "operation": operation_value });
    if let Some(result) = operation.result.clone() {
        structured["result"] = result;
    }
    if let Some(job_summaries) = current_jobs {
        structured["current_jobs"] = json!(job_summaries);
    }
    Ok(tool_result(
        render_run_operation_summary(&operation),
        structured,
        false,
    ))
}

async fn operations_list_tool(arguments: Value) -> Result<Value> {
    let status_filter = arguments
        .get("status")
        .and_then(Value::as_str)
        .map(parse_operation_status_filter)
        .transpose()?;
    let active_only = arguments
        .get("active_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(20);

    let views = run_operations().list_status_views().await;
    let total_operations = views.len();
    let operations = views
        .into_iter()
        .filter(|view| !active_only || view.operation.status == "running")
        .filter(|view| status_filter.is_none_or(|status| status.matches(view.operation.status)))
        .take(limit)
        .collect::<Vec<_>>();
    let returned_operations = operations.len();

    let mut operation_values = Vec::with_capacity(operations.len());
    for view in &operations {
        let mut value = operation_status_value(
            &view.operation,
            view.age_seconds,
            view.last_update_age_seconds,
            view.progress_percent,
            view.is_stale,
        )?;
        if let Some(current_jobs) = view.current_jobs.clone() {
            value["current_jobs"] = json!(current_jobs);
        }
        operation_values.push(value);
    }

    let filter_summary = operations_filter_summary(status_filter, active_only, limit);
    let text = if operation_values.is_empty() {
        format!("No Opal background operations matched {filter_summary}")
    } else {
        let details = operation_values
            .iter()
            .map(render_operations_list_line)
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Found {} Opal background operation(s) matching {}:\n{}",
            returned_operations, filter_summary, details
        )
    };

    Ok(tool_result(
        text,
        json!({
            "operations": operation_values,
            "filters": {
                "status": status_filter.map(OperationStatusFilter::as_str),
                "active_only": active_only,
                "limit": limit,
            },
            "total_operations": total_operations,
            "returned_operations": returned_operations,
        }),
        false,
    ))
}

async fn view_tool(app: &OpalApp, arguments: Value) -> Value {
    let request = match view_request_from_value(app, arguments) {
        Ok(request) => request,
        Err(err) => return error_tool_result(err.to_string(), Value::Null),
    };
    if !request.include_log && !request.include_runtime_summary {
        return match view_tool_result(&request).await {
            Ok(payload) => tool_result(payload.text, payload.structured, false),
            Err(err) => error_tool_result(err.to_string(), Value::Null),
        };
    }
    let start = run_operations().start(
        RunOperationRequest {
            tool: "opal_view",
            dedupe_key: Some(operation_dedupe_key(
                "opal_view",
                [
                    ("workdir", Some(request.workdir.display().to_string())),
                    ("run_id", request.run_id.clone()),
                    ("job", request.job_name.clone()),
                    ("include_log", Some(request.include_log.to_string())),
                    (
                        "include_runtime_summary",
                        Some(request.include_runtime_summary.to_string()),
                    ),
                ],
            )),
            workdir: Some(request.workdir.display().to_string()),
            pipeline: None,
            resolved_engine: None,
            run_id: request.run_id.clone(),
            requested_jobs: request.job_name.iter().cloned().collect(),
            requested_job: request.job_name.clone(),
            source_run_id: request.run_id.clone(),
        },
        async move { execute_view_request(request).await },
    );
    let operation = start.operation;
    tool_result(
        if start.reused_existing {
            format!(
                "Reusing background view operation {}. Poll opal_run_status with this operation_id.",
                operation.operation_id
            )
        } else {
            format!(
                "Started background view operation {}. Poll opal_run_status with this operation_id.",
                operation.operation_id
            )
        },
        json!({
            "operation": operation,
            "deduped": start.reused_existing,
        }),
        false,
    )
}

async fn view_tool_result(request: &ViewRequest) -> Result<ToolResultPayload> {
    let entry =
        selected_history_entry_for_workdir(&request.workdir, request.run_id.as_deref()).await?;
    let mut structured = Map::new();
    structured.insert("run".to_string(), history_entry_json(&entry));

    let mut text = format!(
        "Opal run {} finished at {} with status {:?}",
        entry.run_id, entry.finished_at, entry.status
    );

    if let Some(job_name) = request.job_name.as_deref() {
        let job = find_job(&entry, job_name)
            .with_context(|| format!("job '{job_name}' not found in run '{}'", entry.run_id))?;
        structured.insert("job".to_string(), json!(job));
        text.push_str(&format!("\nSelected job: {} ({:?})", job.name, job.status));
        if request.include_log {
            let log = read_job_log(&entry, job).await?;
            structured.insert("job_log".to_string(), json!(log));
            text.push_str("\nIncluded job log.");
        }
        if request.include_runtime_summary {
            let summary = read_runtime_summary(job).await?;
            structured.insert("runtime_summary".to_string(), json!(summary));
            text.push_str("\nIncluded runtime summary.");
        }
    } else {
        text.push_str(&format!("\nJobs recorded: {}", entry.jobs.len()));
    }

    Ok(ToolResultPayload {
        text,
        structured: Value::Object(structured),
    })
}

async fn failed_jobs_tool(app: &OpalApp, arguments: Value) -> Result<Value> {
    let run_id = arguments.get("run_id").and_then(Value::as_str);
    let entry = selected_history_entry_for_arguments(app, &arguments, run_id).await?;
    let failed_jobs = failed_jobs(&entry);
    let failed_names = failed_jobs
        .iter()
        .map(|job| job.name.clone())
        .collect::<Vec<_>>();
    let text = if failed_names.is_empty() {
        format!("Opal run {} has no failed jobs", entry.run_id)
    } else {
        format!(
            "Opal run {} has {} failed job(s): {}",
            entry.run_id,
            failed_names.len(),
            failed_names.join(", ")
        )
    };

    Ok(tool_result(
        text,
        json!({
            "run": history_entry_json(&entry),
            "failed_jobs": failed_jobs,
            "failed_job_names": failed_names,
        }),
        false,
    ))
}

async fn history_list_tool(app: &OpalApp, arguments: Value) -> Result<Value> {
    let status = history_status_from_value(arguments.get("status"))?;
    let job_name = arguments.get("job").and_then(Value::as_str);
    let branch = arguments.get("branch").and_then(Value::as_str);
    let pipeline_file = arguments.get("pipeline_file").and_then(Value::as_str);
    let since = history_time_filter(arguments.get("since"), "since")?;
    let until = history_time_filter(arguments.get("until"), "until")?;
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(20);

    let history = load_history_for_workdir(&requested_workdir(app, &arguments)).await?;
    let total_runs = history.len();
    let runs = history
        .into_iter()
        .rev()
        .filter(|entry| matches_status(entry, status))
        .filter(|entry| matches_job_name(entry, job_name))
        .filter(|entry| matches_branch(entry, branch))
        .filter(|entry| matches_pipeline_file(entry, pipeline_file))
        .filter(|entry| matches_finished_at(entry, since.as_deref(), until.as_deref()))
        .take(limit)
        .collect::<Vec<_>>();

    let filter_summary = history_filter_summary(
        status,
        job_name,
        branch,
        pipeline_file,
        since.as_deref(),
        until.as_deref(),
        limit,
    );
    let text = if runs.is_empty() {
        format!("No recorded Opal runs matched {filter_summary}")
    } else {
        format!(
            "Found {} recorded Opal run(s) matching {}",
            runs.len(),
            filter_summary
        )
    };

    Ok(tool_result(
        text,
        json!({
            "runs": runs,
            "filters": {
                "status": status.map(history_status_label),
                "job": job_name,
                "branch": branch,
                "pipeline_file": pipeline_file,
                "since": since,
                "until": until,
                "limit": limit,
            },
            "total_runs": total_runs,
            "returned_runs": runs.len(),
        }),
        false,
    ))
}

async fn run_diff_tool(app: &OpalApp, arguments: Value) -> Result<Value> {
    let run_id = arguments.get("run_id").and_then(Value::as_str);
    let base_run_id = arguments.get("base_run_id").and_then(Value::as_str);
    let history = load_history_for_workdir(&requested_workdir(app, &arguments)).await?;
    let (base_run, head_run) = selected_run_pair(&history, run_id, base_run_id)?;
    let diff = compare_runs(&base_run, &head_run);
    Ok(tool_result(
        render_run_diff_summary(&diff),
        json!(diff),
        false,
    ))
}

async fn logs_search_tool(app: &OpalApp, arguments: Value) -> Value {
    let request = match log_search_request_from_value(app, arguments) {
        Ok(request) => request,
        Err(err) => return error_tool_result(err.to_string(), Value::Null),
    };
    let start = run_operations().start(
        RunOperationRequest {
            tool: "opal_logs_search",
            dedupe_key: Some(operation_dedupe_key(
                "opal_logs_search",
                [
                    ("workdir", Some(request.workdir.display().to_string())),
                    ("run_id", request.run_id.clone()),
                    ("job", request.job_name.clone()),
                    ("query", Some(request.query.clone())),
                    ("case_sensitive", Some(request.case_sensitive.to_string())),
                    ("limit", Some(request.limit.to_string())),
                ],
            )),
            workdir: Some(request.workdir.display().to_string()),
            pipeline: None,
            resolved_engine: None,
            run_id: request.run_id.clone(),
            requested_jobs: request.job_name.iter().cloned().collect(),
            requested_job: request.job_name.clone(),
            source_run_id: request.run_id.clone(),
        },
        async move { execute_log_search(request).await },
    );
    let operation = start.operation;
    tool_result(
        if start.reused_existing {
            format!(
                "Reusing background log search operation {}. Poll opal_run_status with this operation_id.",
                operation.operation_id
            )
        } else {
            format!(
                "Started background log search operation {}. Poll opal_run_status with this operation_id.",
                operation.operation_id
            )
        },
        json!({
            "operation": operation,
            "deduped": start.reused_existing,
        }),
        false,
    )
}

async fn log_search_tool_result(request: &LogSearchRequest) -> Result<ToolResultPayload> {
    let history = load_history_for_workdir(&request.workdir).await?;
    let query = request.query.as_str();
    let mut matches = Vec::new();
    let mut read_errors = Vec::new();
    let mut scanned_jobs = 0usize;

    for entry in history.iter().rev() {
        if request
            .run_id
            .as_deref()
            .is_some_and(|run_id| entry.run_id != run_id)
        {
            continue;
        }
        for job in &entry.jobs {
            if request
                .job_name
                .as_deref()
                .is_some_and(|job_name| job.name != job_name)
            {
                continue;
            }
            scanned_jobs += 1;
            match read_job_log(entry, job).await {
                Ok(log) => {
                    if let Some(log_match) =
                        find_log_match(entry, job, &log, query, request.case_sensitive)
                    {
                        matches.push(log_match);
                        if matches.len() >= request.limit {
                            break;
                        }
                    }
                }
                Err(err) => read_errors.push(LogSearchReadError {
                    run_id: entry.run_id.clone(),
                    job: job.name.clone(),
                    error: err.to_string(),
                }),
            }
        }
        if matches.len() >= request.limit {
            break;
        }
    }

    let text = if matches.is_empty() {
        format!("No recorded job logs matched query '{query}'")
    } else {
        format!(
            "Found {} log match(es) for query '{}' across {} scanned job(s)",
            matches.len(),
            query,
            scanned_jobs
        )
    };

    let structured = json!({
        "query": query,
        "case_sensitive": request.case_sensitive,
        "filters": {
            "run_id": request.run_id.clone(),
            "job": request.job_name.clone(),
            "limit": request.limit,
        },
        "matches": matches,
        "returned_matches": matches.len(),
        "scanned_jobs": scanned_jobs,
        "read_errors": read_errors,
    });

    Ok(ToolResultPayload { text, structured })
}

fn log_search_request_from_value(app: &OpalApp, arguments: Value) -> Result<LogSearchRequest> {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(ToOwned::to_owned)
        .context("missing query")?;
    let run_id = arguments
        .get("run_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let job_name = arguments
        .get("job")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(20);
    let case_sensitive = arguments
        .get("case_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    Ok(LogSearchRequest {
        workdir: requested_workdir(app, &arguments),
        query,
        run_id,
        job_name,
        limit,
        case_sensitive,
    })
}

async fn execute_log_search(request: LogSearchRequest) -> RunCapture {
    match log_search_tool_result(&request).await {
        Ok(payload) => RunCapture {
            history_entry: None,
            error: None,
            result: Some(payload.structured),
            result_summary: Some(payload.text),
        },
        Err(err) => RunCapture {
            history_entry: None,
            error: Some(err.to_string()),
            result: None,
            result_summary: None,
        },
    }
}

async fn job_rerun_tool(app: &OpalApp, arguments: Value) -> Value {
    let request = match job_rerun_request(app, &arguments).await {
        Ok(request) => request,
        Err(err) => return error_tool_result(err.to_string(), Value::Null),
    };
    let run_args = request.run_args;
    let resolved_workdir = app.resolve_workdir(run_args.workdir.clone());
    let resolved_pipeline = resolve_pipeline_path(&resolved_workdir, run_args.pipeline.clone());
    let requested_job = request.requested_job.clone();
    let source_run_id = request.source_run.run_id.clone();
    let resolved_engine = resolved_engine_label(&resolved_workdir, run_args.engine).await;
    let operations = run_operations();
    let start =
        operations.start_with_operation_id(
            RunOperationRequest {
                tool: "opal_job_rerun",
                dedupe_key: Some(operation_dedupe_key(
                    "opal_job_rerun",
                    [
                        ("workdir", Some(resolved_workdir.display().to_string())),
                        ("pipeline", Some(resolved_pipeline.display().to_string())),
                        ("job", Some(requested_job.clone())),
                        ("source_run_id", Some(source_run_id.clone())),
                    ],
                )),
                workdir: Some(resolved_workdir.display().to_string()),
                pipeline: Some(resolved_pipeline.display().to_string()),
                resolved_engine,
                run_id: Some(source_run_id.clone()),
                requested_jobs: vec![requested_job.clone()],
                requested_job: Some(requested_job.clone()),
                source_run_id: Some(source_run_id.clone()),
            },
            {
                let app = app.clone();
                let operations = operations.clone();
                move |operation_id| {
                    let progress = operation_progress_callback(operations, operation_id);
                    async move {
                        execute_and_capture_with_progress(&app, run_args, Some(progress)).await
                    }
                }
            },
        );
    let operation = start.operation;
    tool_result(
        if start.reused_existing {
            format!(
                "Reusing background rerun operation {} for job {} from recorded run {}. Poll opal_run_status with this operation_id.",
                operation.operation_id, requested_job, source_run_id
            )
        } else {
            format!(
                "Started background rerun operation {} for job {} from recorded run {}. Poll opal_run_status with this operation_id.",
                operation.operation_id, requested_job, source_run_id
            )
        },
        json!({
            "operation": operation,
            "source_run": history_entry_json(&request.source_run),
            "source_job": request.source_job,
            "requested_job": requested_job,
            "deduped": start.reused_existing,
        }),
        false,
    )
}

async fn plan_explain_tool(app: &OpalApp, arguments: Value) -> Result<Value> {
    let job = arguments
        .get("job")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .context("missing job")?;
    let explanation = explain_plan(app, plan_args_from_value(arguments)?, &job).await?;
    Ok(tool_result(
        explanation.summary,
        json!(explanation.details),
        false,
    ))
}

async fn engine_status_tool(app: &OpalApp, arguments: Value) -> Result<Value> {
    let workdir = app.resolve_workdir(
        arguments
            .get("workdir")
            .and_then(Value::as_str)
            .map(PathBuf::from),
    );
    let settings = OpalConfig::load_async(&workdir).await?;
    let configured_default = settings.default_engine().unwrap_or(EngineChoice::Auto);
    let resolved_auto = resolved_engine_choice(EngineChoice::Auto, &settings);
    let engines = [
        EngineChoice::Container,
        EngineChoice::Docker,
        EngineChoice::Podman,
        EngineChoice::Nerdctl,
        EngineChoice::Orbstack,
        EngineChoice::Sandbox,
    ]
    .into_iter()
    .map(engine_status_entry)
    .collect::<Vec<_>>();

    let available = engines
        .iter()
        .filter(|engine| engine.available && engine.supported)
        .map(|engine| engine.choice.to_string())
        .collect::<Vec<_>>();
    let summary = format!(
        "Configured default engine is {}; auto resolves to {}; available supported engines: {}",
        configured_default.as_str(),
        engine_kind_name(resolved_auto),
        if available.is_empty() {
            "none".to_string()
        } else {
            available.join(", ")
        }
    );

    Ok(tool_result(
        summary,
        json!({
            "configured_default": configured_default.as_str(),
            "resolved_auto": engine_kind_name(resolved_auto),
            "engines": engines,
        }),
        false,
    ))
}

fn tool_result(text: String, structured_content: Value, is_error: bool) -> Value {
    let mut result = json!({
        "content": [{
            "type": "text",
            "text": text
        }],
        "isError": is_error
    });
    if !structured_content.is_null() {
        result["structuredContent"] = structured_content;
    }
    result
}

fn error_tool_result(text: String, structured_content: Value) -> Value {
    tool_result(text, structured_content, true)
}

#[derive(Debug, Clone, serde::Serialize)]
struct RunOperation {
    operation_id: String,
    tool: String,
    status: &'static str,
    phase: &'static str,
    started_at: String,
    #[serde(skip_serializing)]
    started_at_unix: i64,
    last_update_at: String,
    #[serde(skip_serializing)]
    last_update_at_unix: i64,
    finished_at: Option<String>,
    workdir: Option<String>,
    pipeline: Option<String>,
    resolved_engine: Option<String>,
    run_id: Option<String>,
    active_job: Option<String>,
    completed_jobs: usize,
    failed_jobs: usize,
    total_jobs: Option<usize>,
    requested_jobs: Vec<String>,
    requested_job: Option<String>,
    source_run_id: Option<String>,
    run: Option<HistoryEntry>,
    result: Option<Value>,
    result_summary: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct RunOperationRequest {
    tool: &'static str,
    dedupe_key: Option<String>,
    workdir: Option<String>,
    pipeline: Option<String>,
    resolved_engine: Option<String>,
    run_id: Option<String>,
    requested_jobs: Vec<String>,
    requested_job: Option<String>,
    source_run_id: Option<String>,
}

#[derive(Debug, Clone)]
struct RunOperationStart {
    operation: RunOperation,
    reused_existing: bool,
}

#[derive(Debug, Clone)]
struct RunOperationStatusView {
    operation: RunOperation,
    age_seconds: i64,
    last_update_age_seconds: i64,
    progress_percent: u8,
    is_stale: bool,
    current_jobs: Option<Vec<RunOperationJobSummary>>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RunOperationJobSummary {
    name: String,
    stage: String,
    status: &'static str,
}

struct RunOperationState {
    operation: RunOperation,
    dedupe_key: Option<String>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

enum InsertStartOperation {
    Started {
        operation: RunOperation,
        operation_id: String,
    },
    Reused(RunOperation),
}

#[derive(Clone, Default)]
struct RunOperations {
    inner: Arc<Mutex<BTreeMap<String, RunOperationState>>>,
}

impl RunOperations {
    fn start<F>(&self, request: RunOperationRequest, future: F) -> RunOperationStart
    where
        F: std::future::Future<Output = RunCapture> + Send + 'static,
    {
        self.start_with_operation_id(request, move |_| future)
    }

    fn start_with_operation_id<F, B>(
        &self,
        request: RunOperationRequest,
        future_builder: B,
    ) -> RunOperationStart
    where
        F: std::future::Future<Output = RunCapture> + Send + 'static,
        B: FnOnce(String) -> F,
    {
        let (operation, operation_id) = match self.insert_start_operation(request) {
            InsertStartOperation::Started {
                operation,
                operation_id,
            } => (operation, operation_id),
            InsertStartOperation::Reused(operation) => {
                return RunOperationStart {
                    operation,
                    reused_existing: true,
                };
            }
        };

        let future = future_builder(operation_id.clone());
        let operations = self.clone();
        let operation_id_for_task = operation_id.clone();
        let task_handle = tokio::spawn(async move {
            operations.update_phase(&operation_id_for_task, "preparing");
            operations.update_phase(&operation_id_for_task, "executing");
            let mut future = std::pin::Pin::from(Box::new(future));
            loop {
                tokio::select! {
                    capture = &mut future => {
                        operations.update_phase(&operation_id_for_task, "finalizing");
                        operations.finish(&operation_id_for_task, capture);
                        break;
                    }
                    _ = tokio::time::sleep(RUN_OPERATION_HEARTBEAT_INTERVAL) => {
                        operations.heartbeat(&operation_id_for_task);
                    }
                }
            }
        });
        self.set_task_handle(&operation_id, task_handle);

        RunOperationStart {
            operation,
            reused_existing: false,
        }
    }

    fn insert_start_operation(&self, request: RunOperationRequest) -> InsertStartOperation {
        if let Some(operation) = self.find_running_duplicate(request.dedupe_key.as_deref()) {
            return InsertStartOperation::Reused(operation);
        }

        let now = now_rfc3339();
        let now_unix = now_unix_seconds();
        let operation = RunOperation {
            operation_id: next_operation_id(),
            tool: request.tool.to_string(),
            status: "running",
            phase: "queued",
            started_at: now.clone(),
            started_at_unix: now_unix,
            last_update_at: now,
            last_update_at_unix: now_unix,
            finished_at: None,
            workdir: request.workdir,
            pipeline: request.pipeline,
            resolved_engine: request.resolved_engine,
            run_id: request.run_id,
            active_job: request.requested_job.clone(),
            completed_jobs: 0,
            failed_jobs: 0,
            total_jobs: None,
            requested_jobs: request.requested_jobs,
            requested_job: request.requested_job,
            source_run_id: request.source_run_id,
            run: None,
            result: None,
            result_summary: None,
            error: None,
        };
        let operation_id = operation.operation_id.clone();
        let mut operations = self.inner.lock().expect("run operations lock");
        operations.insert(
            operation_id.clone(),
            RunOperationState {
                operation: operation.clone(),
                dedupe_key: request.dedupe_key,
                task_handle: None,
            },
        );
        InsertStartOperation::Started {
            operation,
            operation_id,
        }
    }

    async fn status_view(&self, operation_id: &str) -> Option<RunOperationStatusView> {
        self.reconcile_finished_task(operation_id).await;
        self.fail_stale(operation_id);
        self.get(operation_id).map(|operation| {
            let now = now_unix_seconds();
            let age_seconds = (now - operation.started_at_unix).max(0);
            let last_update_age_seconds = (now - operation.last_update_at_unix).max(0);
            let is_stale = operation.status == "running"
                && last_update_age_seconds >= RUN_OPERATION_STALE_AFTER.as_secs() as i64;
            let current_jobs = operation.run.as_ref().map(run_job_summaries);
            let progress_percent = operation_progress_percent(&operation);
            RunOperationStatusView {
                operation,
                age_seconds,
                last_update_age_seconds,
                progress_percent,
                is_stale,
                current_jobs,
            }
        })
    }

    async fn list_status_views(&self) -> Vec<RunOperationStatusView> {
        let operation_ids = self
            .inner
            .lock()
            .ok()
            .map(|operations| operations.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut views = Vec::new();
        for operation_id in operation_ids {
            if let Some(view) = self.status_view(&operation_id).await {
                views.push(view);
            }
        }
        views.sort_by_key(|view| std::cmp::Reverse(view.operation.started_at_unix));
        views
    }

    fn get(&self, operation_id: &str) -> Option<RunOperation> {
        self.inner.lock().ok().and_then(|operations| {
            operations
                .get(operation_id)
                .map(|state| state.operation.clone())
        })
    }

    fn finish(&self, operation_id: &str, capture: RunCapture) {
        let Ok(mut operations) = self.inner.lock() else {
            return;
        };
        let Some(state) = operations.get_mut(operation_id) else {
            return;
        };
        state.task_handle = None;
        let operation = &mut state.operation;
        let now = now_rfc3339();
        let now_unix = now_unix_seconds();
        operation.finished_at = Some(now.clone());
        operation.last_update_at = now;
        operation.last_update_at_unix = now_unix;
        operation.phase = "completed";
        operation.status = if capture.error.is_some() {
            "failed"
        } else {
            "succeeded"
        };
        operation.active_job = None;
        operation.run = capture.history_entry;
        operation.run_id = operation.run.as_ref().map(|entry| entry.run_id.clone());
        if let Some(entry) = operation.run.as_ref() {
            operation.total_jobs = Some(entry.jobs.len());
            operation.failed_jobs = entry
                .jobs
                .iter()
                .filter(|job| job.status == HistoryStatus::Failed)
                .count();
            operation.completed_jobs = entry
                .jobs
                .iter()
                .filter(|job| job.status != HistoryStatus::Running)
                .count();
        } else {
            operation.total_jobs =
                (!operation.requested_jobs.is_empty()).then_some(operation.requested_jobs.len());
            operation.completed_jobs = operation.total_jobs.unwrap_or_default();
            operation.failed_jobs = usize::from(capture.error.is_some());
        }
        operation.result = capture.result;
        operation.result_summary = capture.result_summary;
        operation.error = capture.error;
    }

    fn find_running_duplicate(&self, dedupe_key: Option<&str>) -> Option<RunOperation> {
        let dedupe_key = dedupe_key?;
        self.inner.lock().ok().and_then(|operations| {
            operations
                .values()
                .find(|state| {
                    state.dedupe_key.as_deref() == Some(dedupe_key)
                        && state.operation.status == "running"
                        && state
                            .task_handle
                            .as_ref()
                            .is_none_or(|handle| !handle.is_finished())
                })
                .map(|state| state.operation.clone())
        })
    }

    fn set_task_handle(&self, operation_id: &str, handle: tokio::task::JoinHandle<()>) {
        let Ok(mut operations) = self.inner.lock() else {
            handle.abort();
            return;
        };
        let Some(state) = operations.get_mut(operation_id) else {
            handle.abort();
            return;
        };
        state.task_handle = Some(handle);
    }

    fn update_phase(&self, operation_id: &str, phase: &'static str) {
        let Ok(mut operations) = self.inner.lock() else {
            return;
        };
        let Some(state) = operations.get_mut(operation_id) else {
            return;
        };
        if state.operation.status == "running" {
            state.operation.phase = phase;
            state.operation.last_update_at = now_rfc3339();
            state.operation.last_update_at_unix = now_unix_seconds();
        }
    }

    fn heartbeat(&self, operation_id: &str) {
        let Ok(mut operations) = self.inner.lock() else {
            return;
        };
        let Some(state) = operations.get_mut(operation_id) else {
            return;
        };
        if state.operation.status == "running" {
            state.operation.last_update_at = now_rfc3339();
            state.operation.last_update_at_unix = now_unix_seconds();
        }
    }

    fn apply_progress_event(&self, operation_id: &str, event: ExecutionProgressEvent) {
        let Ok(mut operations) = self.inner.lock() else {
            return;
        };
        let Some(state) = operations.get_mut(operation_id) else {
            return;
        };
        if state.operation.status != "running" {
            return;
        }
        let operation = &mut state.operation;
        match event {
            ExecutionProgressEvent::PlanPrepared { run_id, total_jobs } => {
                operation.run_id = Some(run_id);
                operation.total_jobs = Some(total_jobs);
            }
            ExecutionProgressEvent::JobStarted { name, .. } => {
                operation.active_job = Some(name);
            }
            ExecutionProgressEvent::JobFinished { status, .. } => {
                operation.active_job = None;
                if let Some(total_jobs) = operation.total_jobs {
                    operation.completed_jobs = (operation.completed_jobs + 1).min(total_jobs);
                } else {
                    operation.completed_jobs += 1;
                }
                if status == ProgressJobStatus::Failed {
                    operation.failed_jobs += 1;
                }
            }
        }
        operation.last_update_at = now_rfc3339();
        operation.last_update_at_unix = now_unix_seconds();
    }

    fn take_finished_task(&self, operation_id: &str) -> Option<tokio::task::JoinHandle<()>> {
        let Ok(mut operations) = self.inner.lock() else {
            return None;
        };
        let state = operations.get_mut(operation_id)?;
        if state.operation.status != "running" {
            return None;
        }
        if state
            .task_handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            return state.task_handle.take();
        }
        None
    }

    async fn reconcile_finished_task(&self, operation_id: &str) {
        let Some(handle) = self.take_finished_task(operation_id) else {
            return;
        };
        if let Err(error) = handle.await {
            self.fail_operation(
                operation_id,
                format!(
                    "background task failed: {}",
                    if error.is_panic() {
                        "task panicked"
                    } else {
                        "task cancelled"
                    }
                ),
            );
        }
    }

    fn fail_stale(&self, operation_id: &str) {
        let stale_for_seconds = {
            let Ok(operations) = self.inner.lock() else {
                return;
            };
            let Some(state) = operations.get(operation_id) else {
                return;
            };
            if state.operation.status != "running" {
                return;
            }
            let last_update_age = (now_unix_seconds() - state.operation.last_update_at_unix).max(0);
            if last_update_age < RUN_OPERATION_STALE_AFTER.as_secs() as i64 {
                return;
            }
            last_update_age
        };
        self.fail_operation(
            operation_id,
            format!(
                "background task heartbeat became stale after {stale_for_seconds}s without progress updates"
            ),
        );
    }

    fn fail_operation(&self, operation_id: &str, error: String) {
        let Ok(mut operations) = self.inner.lock() else {
            return;
        };
        let Some(state) = operations.get_mut(operation_id) else {
            return;
        };
        if state.operation.status != "running" {
            return;
        }
        if let Some(handle) = state.task_handle.take() {
            handle.abort();
        }
        let operation = &mut state.operation;
        let now = now_rfc3339();
        let now_unix = now_unix_seconds();
        operation.status = "failed";
        operation.phase = "completed";
        operation.finished_at = Some(now.clone());
        operation.last_update_at = now;
        operation.last_update_at_unix = now_unix;
        operation.active_job = None;
        operation.error = Some(error);
        operation.failed_jobs = usize::max(operation.failed_jobs, 1);
    }

    #[cfg(test)]
    fn clear(&self) {
        if let Ok(mut operations) = self.inner.lock() {
            for state in operations.values_mut() {
                if let Some(handle) = state.task_handle.take() {
                    handle.abort();
                }
            }
            operations.clear();
        }
    }

    #[cfg(test)]
    fn set_last_update_at_for_test(
        &self,
        operation_id: &str,
        timestamp: &str,
        unix_timestamp: i64,
    ) {
        let Ok(mut operations) = self.inner.lock() else {
            return;
        };
        if let Some(state) = operations.get_mut(operation_id) {
            state.operation.last_update_at = timestamp.to_string();
            state.operation.last_update_at_unix = unix_timestamp;
        }
    }
}

fn run_operations() -> RunOperations {
    static RUN_OPERATIONS: OnceLock<RunOperations> = OnceLock::new();
    RUN_OPERATIONS.get_or_init(RunOperations::default).clone()
}

fn next_operation_id() -> String {
    use sha2::{Digest, Sha256};
    use std::process;
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(process::id().to_le_bytes());
    let suffix = format!("{:x}", hasher.finalize());
    format!("op-{}", &suffix[..8])
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

#[derive(Clone, Copy)]
enum OperationStatusFilter {
    Running,
    Succeeded,
    Failed,
}

impl OperationStatusFilter {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    fn matches(self, status: &str) -> bool {
        self.as_str() == status
    }
}

fn parse_operation_status_filter(value: &str) -> Result<OperationStatusFilter> {
    match value {
        "running" => Ok(OperationStatusFilter::Running),
        "succeeded" => Ok(OperationStatusFilter::Succeeded),
        "failed" => Ok(OperationStatusFilter::Failed),
        other => anyhow::bail!(
            "unsupported operation status filter '{other}'; expected running, succeeded, or failed"
        ),
    }
}

fn operations_filter_summary(
    status: Option<OperationStatusFilter>,
    active_only: bool,
    limit: usize,
) -> String {
    let mut filters = Vec::new();
    if let Some(status) = status {
        filters.push(format!("status={}", status.as_str()));
    }
    if active_only {
        filters.push("active_only=true".to_string());
    }
    filters.push(format!("limit={limit}"));
    filters.join(", ")
}

fn operation_status_value(
    operation: &RunOperation,
    age_seconds: i64,
    last_update_age_seconds: i64,
    progress_percent: u8,
    is_stale: bool,
) -> Result<Value> {
    let mut operation_value = serde_json::to_value(operation)?;
    operation_value["age_seconds"] = json!(age_seconds);
    operation_value["last_update_age_seconds"] = json!(last_update_age_seconds);
    operation_value["progress_percent"] = json!(progress_percent);
    operation_value["is_stale"] = json!(is_stale);
    Ok(operation_value)
}

fn render_operations_list_line(operation: &Value) -> String {
    let id = operation
        .get("operation_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let tool = operation
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let status = operation
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let phase = operation
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let progress_percent = operation
        .get("progress_percent")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completed_jobs = operation
        .get("completed_jobs")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_jobs = operation
        .get("total_jobs")
        .and_then(Value::as_u64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string());
    let active_job = operation
        .get("active_job")
        .and_then(Value::as_str)
        .unwrap_or("-");
    format!(
        "{} [{}] {} phase={} progress={} jobs={}/{} active_job={}",
        id, status, tool, phase, progress_percent, completed_jobs, total_jobs, active_job
    )
}

const RUN_OPERATION_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const RUN_OPERATION_STALE_AFTER: Duration = Duration::from_secs(30);

fn now_unix_seconds() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn normalized_jobs_key(jobs: &[String]) -> String {
    let mut jobs = jobs.to_vec();
    jobs.sort();
    jobs.dedup();
    jobs.join(",")
}

fn operation_dedupe_key<I>(tool: &str, attributes: I) -> String
where
    I: IntoIterator<Item = (&'static str, Option<String>)>,
{
    let mut parts = vec![format!("tool={tool}")];
    for (key, value) in attributes {
        parts.push(format!(
            "{key}={}",
            value.unwrap_or_else(|| "<none>".to_string())
        ));
    }
    parts.join("|")
}

fn run_job_summaries(run: &HistoryEntry) -> Vec<RunOperationJobSummary> {
    run.jobs
        .iter()
        .map(|job| RunOperationJobSummary {
            name: job.name.clone(),
            stage: job.stage.clone(),
            status: history_status_label(job.status),
        })
        .collect()
}

async fn resolved_engine_label(workdir: &Path, requested: EngineChoice) -> Option<String> {
    let settings = OpalConfig::load_async(workdir).await.ok()?;
    let resolved = resolve_engine_choice(requested, &settings);
    Some(resolved.as_str().to_string())
}

fn operation_progress_percent(operation: &RunOperation) -> u8 {
    if operation.status != "running" {
        return 100;
    }

    match operation.phase {
        "queued" => 0,
        "preparing" => 10,
        "executing" => match operation.total_jobs {
            Some(total) if total > 0 => {
                let done = operation.completed_jobs.min(total);
                10 + ((done as f32 / total as f32) * 80.0).round() as u8
            }
            _ => 50,
        },
        "finalizing" => 95,
        _ => 0,
    }
}

fn operation_progress_callback(
    operations: RunOperations,
    operation_id: String,
) -> ExecutionProgressCallback {
    Arc::new(move |event| {
        operations.apply_progress_event(&operation_id, event);
    })
}

fn render_run_operation_summary(operation: &RunOperation) -> String {
    match (
        operation.status,
        operation.run.as_ref(),
        operation.result_summary.as_deref(),
        operation.error.as_deref(),
    ) {
        ("running", _, _, _) => format!(
            "Opal background operation {} is running (phase: {})",
            operation.operation_id, operation.phase
        ),
        ("succeeded", Some(run), _, _) => format!(
            "Opal background operation {} completed as run {} with status {:?}",
            operation.operation_id, run.run_id, run.status
        ),
        ("succeeded", None, Some(summary), _) => summary.to_string(),
        ("succeeded", None, None, _) => format!(
            "Opal background operation {} completed without a recorded history entry",
            operation.operation_id
        ),
        ("failed", Some(run), _, Some(error)) => format!(
            "Opal background operation {} finished as run {} with status {:?}: {error}",
            operation.operation_id, run.run_id, run.status
        ),
        ("failed", None, _, Some(error)) => format!(
            "Opal background operation {} failed before recording history: {error}",
            operation.operation_id
        ),
        (_, _, _, Some(error)) => format!(
            "Opal background operation {} finished with error: {error}",
            operation.operation_id
        ),
        _ => format!(
            "Opal background operation {} is in state {}",
            operation.operation_id, operation.status
        ),
    }
}

struct LogSearchRequest {
    workdir: PathBuf,
    query: String,
    run_id: Option<String>,
    job_name: Option<String>,
    limit: usize,
    case_sensitive: bool,
}

struct ViewRequest {
    workdir: PathBuf,
    run_id: Option<String>,
    job_name: Option<String>,
    include_log: bool,
    include_runtime_summary: bool,
}

struct ToolResultPayload {
    text: String,
    structured: Value,
}

fn history_entry_json(entry: &HistoryEntry) -> Value {
    json!(entry)
}

fn requested_workdir(app: &OpalApp, arguments: &Value) -> PathBuf {
    app.resolve_workdir(
        arguments
            .get("workdir")
            .and_then(Value::as_str)
            .map(PathBuf::from),
    )
}

async fn selected_history_entry_for_arguments(
    app: &OpalApp,
    arguments: &Value,
    run_id: Option<&str>,
) -> Result<HistoryEntry> {
    let workdir = requested_workdir(app, arguments);
    selected_history_entry_for_workdir(&workdir, run_id).await
}

async fn selected_history_entry_for_workdir(
    workdir: &Path,
    run_id: Option<&str>,
) -> Result<HistoryEntry> {
    match run_id {
        Some(run_id) => find_history_entry_for_workdir(workdir, run_id)
            .await?
            .with_context(|| format!("run '{run_id}' not found in Opal history")),
        None => latest_history_entry_for_workdir(workdir)
            .await?
            .context("no Opal history entries found"),
    }
}

fn view_request_from_value(app: &OpalApp, arguments: Value) -> Result<ViewRequest> {
    Ok(ViewRequest {
        workdir: requested_workdir(app, &arguments),
        run_id: arguments
            .get("run_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        job_name: arguments
            .get("job")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        include_log: arguments
            .get("include_log")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        include_runtime_summary: arguments
            .get("include_runtime_summary")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

async fn execute_view_request(request: ViewRequest) -> RunCapture {
    match view_tool_result(&request).await {
        Ok(payload) => RunCapture {
            history_entry: None,
            result: Some(payload.structured),
            result_summary: Some(payload.text),
            error: None,
        },
        Err(err) => RunCapture {
            history_entry: None,
            result: None,
            result_summary: None,
            error: Some(err.to_string()),
        },
    }
}

fn failed_jobs(entry: &HistoryEntry) -> Vec<crate::history::HistoryJob> {
    entry
        .jobs
        .iter()
        .filter(|job| job.status == HistoryStatus::Failed)
        .cloned()
        .collect()
}

fn history_status_from_value(value: Option<&Value>) -> Result<Option<HistoryStatus>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(text) = value.as_str() else {
        anyhow::bail!("history status filter must be a string");
    };

    let status = match text.to_ascii_lowercase().as_str() {
        "success" => HistoryStatus::Success,
        "failed" => HistoryStatus::Failed,
        "skipped" => HistoryStatus::Skipped,
        "running" => HistoryStatus::Running,
        other => anyhow::bail!("unsupported history status filter '{other}'"),
    };
    Ok(Some(status))
}

fn history_status_label(status: HistoryStatus) -> &'static str {
    match status {
        HistoryStatus::Success => "success",
        HistoryStatus::Failed => "failed",
        HistoryStatus::Skipped => "skipped",
        HistoryStatus::Running => "running",
    }
}

fn matches_status(entry: &HistoryEntry, status: Option<HistoryStatus>) -> bool {
    status.is_none_or(|status| entry.status == status)
}

fn matches_job_name(entry: &HistoryEntry, job_name: Option<&str>) -> bool {
    job_name.is_none_or(|job_name| entry.jobs.iter().any(|job| job.name == job_name))
}

fn matches_branch(entry: &HistoryEntry, branch: Option<&str>) -> bool {
    branch.is_none_or(|branch| entry.ref_name.as_deref() == Some(branch))
}

fn matches_pipeline_file(entry: &HistoryEntry, pipeline_file: Option<&str>) -> bool {
    pipeline_file.is_none_or(|pipeline_file| entry.pipeline_file.as_deref() == Some(pipeline_file))
}

fn history_filter_summary(
    status: Option<HistoryStatus>,
    job_name: Option<&str>,
    branch: Option<&str>,
    pipeline_file: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    limit: usize,
) -> String {
    let mut filters = Vec::new();
    if let Some(status) = status {
        filters.push(format!("status={}", history_status_label(status)));
    }
    if let Some(job_name) = job_name {
        filters.push(format!("job={job_name}"));
    }
    if let Some(branch) = branch {
        filters.push(format!("branch={branch}"));
    }
    if let Some(pipeline_file) = pipeline_file {
        filters.push(format!("pipeline_file={pipeline_file}"));
    }
    if let Some(since) = since {
        filters.push(format!("since={since}"));
    }
    if let Some(until) = until {
        filters.push(format!("until={until}"));
    }
    filters.push(format!("limit={limit}"));
    filters.join(", ")
}

fn history_time_filter(value: Option<&Value>, key: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(text) = value.as_str() else {
        anyhow::bail!("history {key} filter must be a string");
    };
    if !looks_like_rfc3339_utc(text) {
        anyhow::bail!("history {key} filter must be an RFC3339 UTC timestamp");
    }
    Ok(Some(text.to_string()))
}

fn matches_finished_at(entry: &HistoryEntry, since: Option<&str>, until: Option<&str>) -> bool {
    since.is_none_or(|since| entry.finished_at.as_str() >= since)
        && until.is_none_or(|until| entry.finished_at.as_str() <= until)
}

fn looks_like_rfc3339_utc(value: &str) -> bool {
    value.len() >= 20
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value.as_bytes().get(10) == Some(&b'T')
        && value.as_bytes().get(13) == Some(&b':')
        && value.as_bytes().get(16) == Some(&b':')
        && value.ends_with('Z')
}

#[derive(serde::Serialize)]
struct RunDiffSummary {
    base_run: RunDiffRun,
    head_run: RunDiffRun,
    overall_status_changed: bool,
    changed_jobs: Vec<ChangedRunJob>,
    added_jobs: Vec<RunDiffJob>,
    removed_jobs: Vec<RunDiffJob>,
    unchanged_jobs: usize,
}

#[derive(serde::Serialize)]
struct RunDiffRun {
    run_id: String,
    finished_at: String,
    status: &'static str,
}

#[derive(serde::Serialize)]
struct RunDiffJob {
    name: String,
    stage: String,
    status: &'static str,
}

#[derive(serde::Serialize)]
struct ChangedRunJob {
    name: String,
    previous_stage: String,
    current_stage: String,
    previous_status: &'static str,
    current_status: &'static str,
}

#[derive(serde::Serialize)]
struct LogSearchMatch {
    run_id: String,
    finished_at: String,
    job: String,
    stage: String,
    status: &'static str,
    line_matches: usize,
    matching_lines: Vec<LogSearchLineMatch>,
}

#[derive(serde::Serialize)]
struct LogSearchLineMatch {
    line_number: usize,
    text: String,
}

#[derive(serde::Serialize)]
struct LogSearchReadError {
    run_id: String,
    job: String,
    error: String,
}

struct JobRerunRequest {
    source_run: HistoryEntry,
    source_job: crate::history::HistoryJob,
    requested_job: String,
    run_args: RunArgs,
}

fn selected_run_pair(
    history: &[HistoryEntry],
    run_id: Option<&str>,
    base_run_id: Option<&str>,
) -> Result<(HistoryEntry, HistoryEntry)> {
    if history.is_empty() {
        anyhow::bail!("no Opal history entries found");
    }
    if base_run_id.is_some() && run_id.is_none() {
        anyhow::bail!("run_id is required when base_run_id is provided");
    }

    match (run_id, base_run_id) {
        (Some(run_id), Some(base_run_id)) => Ok((
            history_entry_by_run_id(history, base_run_id)?.clone(),
            history_entry_by_run_id(history, run_id)?.clone(),
        )),
        (Some(run_id), None) => {
            let index = history_index_by_run_id(history, run_id)?;
            let base = history
                .get(
                    index
                        .checked_sub(1)
                        .context("selected run has no previous recorded run")?,
                )
                .context("selected run has no previous recorded run")?;
            Ok((base.clone(), history[index].clone()))
        }
        (None, None) => {
            let head_index = history
                .len()
                .checked_sub(1)
                .context("no Opal history entries found")?;
            let base_index = head_index
                .checked_sub(1)
                .context("need at least two recorded runs to compare history")?;
            Ok((history[base_index].clone(), history[head_index].clone()))
        }
        (None, Some(_)) => unreachable!("checked above"),
    }
}

fn history_entry_by_run_id<'a>(
    history: &'a [HistoryEntry],
    run_id: &str,
) -> Result<&'a HistoryEntry> {
    let index = history_index_by_run_id(history, run_id)?;
    Ok(&history[index])
}

fn history_index_by_run_id(history: &[HistoryEntry], run_id: &str) -> Result<usize> {
    history
        .iter()
        .position(|entry| entry.run_id == run_id)
        .with_context(|| format!("run '{run_id}' not found in Opal history"))
}

fn compare_runs(base_run: &HistoryEntry, head_run: &HistoryEntry) -> RunDiffSummary {
    let base_jobs = base_run
        .jobs
        .iter()
        .map(|job| (job.name.as_str(), job))
        .collect::<BTreeMap<_, _>>();
    let head_jobs = head_run
        .jobs
        .iter()
        .map(|job| (job.name.as_str(), job))
        .collect::<BTreeMap<_, _>>();
    let names = base_jobs
        .keys()
        .chain(head_jobs.keys())
        .copied()
        .collect::<BTreeSet<_>>();

    let mut changed_jobs = Vec::new();
    let mut added_jobs = Vec::new();
    let mut removed_jobs = Vec::new();
    let mut unchanged_jobs = 0usize;

    for name in names {
        match (base_jobs.get(name), head_jobs.get(name)) {
            (Some(previous), Some(current)) => {
                if previous.status != current.status || previous.stage != current.stage {
                    changed_jobs.push(ChangedRunJob {
                        name: name.to_string(),
                        previous_stage: previous.stage.clone(),
                        current_stage: current.stage.clone(),
                        previous_status: history_status_label(previous.status),
                        current_status: history_status_label(current.status),
                    });
                } else {
                    unchanged_jobs += 1;
                }
            }
            (None, Some(current)) => added_jobs.push(run_diff_job(current)),
            (Some(previous), None) => removed_jobs.push(run_diff_job(previous)),
            (None, None) => {}
        }
    }

    RunDiffSummary {
        base_run: run_diff_run(base_run),
        head_run: run_diff_run(head_run),
        overall_status_changed: base_run.status != head_run.status,
        changed_jobs,
        added_jobs,
        removed_jobs,
        unchanged_jobs,
    }
}

fn run_diff_run(run: &HistoryEntry) -> RunDiffRun {
    RunDiffRun {
        run_id: run.run_id.clone(),
        finished_at: run.finished_at.clone(),
        status: history_status_label(run.status),
    }
}

fn run_diff_job(job: &crate::history::HistoryJob) -> RunDiffJob {
    RunDiffJob {
        name: job.name.clone(),
        stage: job.stage.clone(),
        status: history_status_label(job.status),
    }
}

fn render_run_diff_summary(diff: &RunDiffSummary) -> String {
    let mut lines = vec![format!(
        "Compared run {} against {}",
        diff.head_run.run_id, diff.base_run.run_id
    )];
    if diff.overall_status_changed {
        lines.push(format!(
            "Overall status changed: {} -> {}",
            diff.base_run.status, diff.head_run.status
        ));
    } else {
        lines.push(format!("Overall status stayed {}", diff.head_run.status));
    }
    lines.push(format!(
        "Job changes: {} changed, {} added, {} removed, {} unchanged",
        diff.changed_jobs.len(),
        diff.added_jobs.len(),
        diff.removed_jobs.len(),
        diff.unchanged_jobs
    ));
    if !diff.changed_jobs.is_empty() {
        lines.push(format!(
            "Changed jobs: {}",
            diff.changed_jobs
                .iter()
                .map(|job| job.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !diff.added_jobs.is_empty() {
        lines.push(format!(
            "Added jobs: {}",
            diff.added_jobs
                .iter()
                .map(|job| job.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !diff.removed_jobs.is_empty() {
        lines.push(format!(
            "Removed jobs: {}",
            diff.removed_jobs
                .iter()
                .map(|job| job.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines.join("\n")
}

fn find_log_match(
    entry: &HistoryEntry,
    job: &crate::history::HistoryJob,
    log: &str,
    query: &str,
    case_sensitive: bool,
) -> Option<LogSearchMatch> {
    let mut matching_lines = Vec::new();
    let mut line_matches = 0usize;
    for (index, line) in log.lines().enumerate() {
        if line_matches_query(line, query, case_sensitive) {
            line_matches += 1;
            if matching_lines.len() < 3 {
                matching_lines.push(LogSearchLineMatch {
                    line_number: index + 1,
                    text: line.to_string(),
                });
            }
        }
    }
    if line_matches == 0 {
        return None;
    }
    Some(LogSearchMatch {
        run_id: entry.run_id.clone(),
        finished_at: entry.finished_at.clone(),
        job: job.name.clone(),
        stage: job.stage.clone(),
        status: history_status_label(job.status),
        line_matches,
        matching_lines,
    })
}

fn line_matches_query(line: &str, query: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        line.contains(query)
    } else {
        line.to_ascii_lowercase()
            .contains(&query.to_ascii_lowercase())
    }
}

async fn job_rerun_request(app: &OpalApp, arguments: &Value) -> Result<JobRerunRequest> {
    let run_id = arguments.get("run_id").and_then(Value::as_str);
    let requested_job = arguments
        .get("job")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .context("missing job")?;
    let source_run = selected_history_entry_for_arguments(app, arguments, run_id).await?;
    let source_job = find_job(&source_run, &requested_job)
        .cloned()
        .with_context(|| {
            format!(
                "job '{}' not found in recorded run '{}'",
                requested_job, source_run.run_id
            )
        })?;
    let mut run_args = run_args_from_value(arguments.clone())?;
    run_args.jobs = vec![requested_job.clone()];

    Ok(JobRerunRequest {
        source_run,
        source_job,
        requested_job,
        run_args,
    })
}

#[derive(serde::Serialize)]
struct EngineStatusEntry {
    choice: &'static str,
    runtime: &'static str,
    binary: &'static str,
    supported: bool,
    available: bool,
    note: Option<String>,
}

impl EngineChoice {
    fn as_str(self) -> &'static str {
        match self {
            EngineChoice::Auto => "auto",
            EngineChoice::Container => "container",
            EngineChoice::Docker => "docker",
            EngineChoice::Podman => "podman",
            EngineChoice::Nerdctl => "nerdctl",
            EngineChoice::Orbstack => "orbstack",
            EngineChoice::Sandbox => "sandbox",
        }
    }
}

fn engine_status_entry(choice: EngineChoice) -> EngineStatusEntry {
    let runtime = resolved_engine_choice(choice, &OpalConfig::default());
    let (supported, note) = engine_support(choice);
    let binary = engine_binary(choice);
    EngineStatusEntry {
        choice: choice.as_str(),
        runtime: engine_kind_name(runtime),
        binary,
        supported,
        available: command_exists(binary),
        note,
    }
}

fn resolved_engine_choice(choice: EngineChoice, settings: &OpalConfig) -> EngineKind {
    let selected = if choice == EngineChoice::Auto {
        settings.default_engine().unwrap_or(EngineChoice::Auto)
    } else {
        choice
    };

    #[cfg(target_os = "macos")]
    {
        match selected {
            EngineChoice::Auto | EngineChoice::Container => EngineKind::ContainerCli,
            EngineChoice::Docker => EngineKind::Docker,
            EngineChoice::Podman => EngineKind::Podman,
            EngineChoice::Nerdctl => EngineKind::Nerdctl,
            EngineChoice::Orbstack => EngineKind::Orbstack,
            EngineChoice::Sandbox => EngineKind::Sandbox,
        }
    }

    #[cfg(target_os = "linux")]
    {
        match selected {
            EngineChoice::Auto | EngineChoice::Podman => EngineKind::Podman,
            EngineChoice::Docker => EngineKind::Docker,
            EngineChoice::Nerdctl => EngineKind::Nerdctl,
            EngineChoice::Orbstack | EngineChoice::Container => EngineKind::Docker,
            EngineChoice::Sandbox => EngineKind::Sandbox,
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = settings;
        let _ = selected;
        EngineKind::Docker
    }
}

fn engine_support(choice: EngineChoice) -> (bool, Option<String>) {
    #[cfg(target_os = "macos")]
    {
        if choice == EngineChoice::Nerdctl {
            return (
                false,
                Some("Opal treats nerdctl as Linux-specific on macOS".to_string()),
            );
        }
        (true, engine_choice_note(choice))
    }

    #[cfg(target_os = "linux")]
    {
        match choice {
            EngineChoice::Container => (
                false,
                Some("Opal falls back to docker when container is selected on Linux".to_string()),
            ),
            EngineChoice::Orbstack => (
                true,
                Some("Opal maps Orbstack to the docker runtime on Linux".to_string()),
            ),
            _ => (true, engine_choice_note(choice)),
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let note = Some("Opal falls back to docker on this platform".to_string());
        let _ = choice;
        (true, note)
    }
}

fn engine_choice_note(choice: EngineChoice) -> Option<String> {
    match choice {
        EngineChoice::Orbstack => Some(
            "Orbstack uses the docker CLI; availability does not distinguish backend identity"
                .to_string(),
        ),
        EngineChoice::Container => {
            Some("Container runtime uses the Apple container CLI".to_string())
        }
        EngineChoice::Sandbox => {
            Some("Sandbox runtime uses the Anthropic Sandbox Runtime CLI (`srt`)".to_string())
        }
        _ => None,
    }
}

fn engine_binary(choice: EngineChoice) -> &'static str {
    match choice {
        EngineChoice::Auto => "docker",
        EngineChoice::Container => "container",
        EngineChoice::Docker | EngineChoice::Orbstack => "docker",
        EngineChoice::Podman => "podman",
        EngineChoice::Nerdctl => "nerdctl",
        EngineChoice::Sandbox => "srt",
    }
}

fn engine_kind_name(kind: EngineKind) -> &'static str {
    match kind {
        EngineKind::ContainerCli => "container",
        EngineKind::Docker => "docker",
        EngineKind::Podman => "podman",
        EngineKind::Nerdctl => "nerdctl",
        EngineKind::Orbstack => "orbstack",
        EngineKind::Sandbox => "sandbox",
    }
}

fn command_exists(program: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&paths).any(|dir| command_in_dir(&dir, program))
}

fn command_in_dir(dir: &Path, program: &str) -> bool {
    let candidate = dir.join(program);
    if candidate.is_file() {
        return true;
    }
    #[cfg(windows)]
    {
        const EXTS: [&str; 3] = ["exe", "cmd", "bat"];
        return EXTS
            .iter()
            .map(|ext| dir.join(format!("{program}.{ext}")))
            .any(|path| path.is_file());
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn plan_args_from_value(value: Value) -> Result<PlanArgs> {
    Ok(PlanArgs {
        pipeline: value
            .get("pipeline")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        workdir: value
            .get("workdir")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        gitlab_base_url: value
            .get("gitlab_base_url")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        gitlab_token: value
            .get("gitlab_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        jobs: string_vec(&value, "jobs"),
        no_pager: true,
        json: value.get("json").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn run_args_from_value(value: Value) -> Result<RunArgs> {
    let engine = value
        .get("engine")
        .and_then(Value::as_str)
        .map(EngineChoice::from_str)
        .transpose()
        .map_err(anyhow::Error::msg)?
        .unwrap_or(EngineChoice::Auto);

    Ok(RunArgs {
        pipeline: value
            .get("pipeline")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        workdir: value
            .get("workdir")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        base_image: value
            .get("base_image")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        env_includes: string_vec(&value, "env_includes"),
        max_parallel_jobs: value
            .get("max_parallel_jobs")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(5),
        trace_scripts: value
            .get("trace_scripts")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        engine,
        no_tui: true,
        gitlab_base_url: value
            .get("gitlab_base_url")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        gitlab_token: value
            .get("gitlab_token")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        rerun_job: value
            .get("rerun_job")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        rerun_run_id: value
            .get("rerun_run_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        jobs: string_vec(&value, "jobs"),
    })
}

fn string_vec(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

#[allow(dead_code)]
fn _view_args_from_value(value: Value) -> ViewArgs {
    ViewArgs {
        workdir: value
            .get("workdir")
            .and_then(Value::as_str)
            .map(PathBuf::from),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExecutionProgressEvent, ProgressJobStatus, RunOperationRequest, call_tool,
        job_rerun_request, list_tools, run_args_from_value, run_operations,
    };
    use crate::app::OpalApp;
    use crate::app::context::history_scope_root;
    use crate::app::run::RunCapture;
    use crate::history::{HistoryEntry, HistoryJob, HistoryStatus, save};
    use crate::mcp::TEST_ENV_LOCK;
    use crate::runtime;
    use serde_json::{Value, json};
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::time::Duration;
    use tempfile::tempdir;
    use time::OffsetDateTime;

    fn current_history_scope() -> String {
        let app = OpalApp::from_current_dir().expect("app");
        history_scope_root(&app.resolve_workdir(None))
    }

    fn wait_for_background_operation(
        runtime: &tokio::runtime::Runtime,
        app: &OpalApp,
        operation_id: &str,
    ) -> Value {
        runtime.block_on(async {
            for _ in 0..50 {
                let status = call_tool(
                    app,
                    json!({
                        "name": "opal_run_status",
                        "arguments": {
                            "operation_id": operation_id
                        }
                    }),
                )
                .await
                .expect("status");
                let state = status["structuredContent"]["operation"]["status"]
                    .as_str()
                    .expect("status");
                if state != "running" {
                    return status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            panic!("background operation did not reach a terminal state");
        })
    }

    fn test_request(dedupe_key: &str) -> RunOperationRequest {
        RunOperationRequest {
            tool: "opal_run",
            dedupe_key: Some(dedupe_key.to_string()),
            workdir: Some("/tmp/opal-test".to_string()),
            pipeline: Some("/tmp/opal-test/.gitlab-ci.yml".to_string()),
            resolved_engine: Some("auto".to_string()),
            run_id: None,
            requested_jobs: vec!["build".to_string()],
            requested_job: None,
            source_run_id: None,
        }
    }

    fn now_minus(seconds: i64) -> (String, i64) {
        let unix = super::now_unix_seconds() - seconds;
        let timestamp = OffsetDateTime::from_unix_timestamp(unix)
            .expect("unix timestamp")
            .format(&time::format_description::well_known::Rfc3339)
            .expect("timestamp");
        (timestamp, unix)
    }

    #[test]
    fn tools_list_exposes_run_plan_and_view() {
        let response = list_tools();
        let tools = response["tools"].as_array().expect("tool array");
        assert!(tools.iter().any(|tool| tool["name"] == "opal_plan"));
        assert!(tools.iter().any(|tool| tool["name"] == "opal_run"));
        assert!(tools.iter().any(|tool| tool["name"] == "opal_run_status"));
        assert!(
            tools
                .iter()
                .any(|tool| tool["name"] == "opal_operations_list")
        );
        assert!(tools.iter().any(|tool| tool["name"] == "opal_view"));
        assert!(tools.iter().any(|tool| tool["name"] == "opal_failed_jobs"));
        assert!(tools.iter().any(|tool| tool["name"] == "opal_history_list"));
        assert!(tools.iter().any(|tool| tool["name"] == "opal_run_diff"));
        assert!(tools.iter().any(|tool| tool["name"] == "opal_logs_search"));
        assert!(tools.iter().any(|tool| tool["name"] == "opal_job_rerun"));
        assert!(tools.iter().any(|tool| tool["name"] == "opal_plan_explain"));
        assert!(
            tools
                .iter()
                .any(|tool| tool["name"] == "opal_engine_status")
        );
    }

    #[test]
    fn run_args_parser_forces_no_tui() {
        let args = run_args_from_value(json!({
            "jobs": ["build"],
            "max_parallel_jobs": 2,
            "engine": "docker"
        }))
        .expect("run args");

        assert!(args.no_tui);
        assert_eq!(args.jobs, vec!["build"]);
        assert_eq!(args.max_parallel_jobs, 2);
    }

    #[test]
    fn run_operations_dedupe_equivalent_requests() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            run_operations().clear();
            let first = run_operations().start(test_request("dedupe-run"), async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                RunCapture {
                    history_entry: None,
                    result: None,
                    result_summary: Some("done".to_string()),
                    error: None,
                }
            });
            let second = run_operations().start(test_request("dedupe-run"), async move {
                RunCapture {
                    history_entry: None,
                    result: None,
                    result_summary: Some("should-not-run".to_string()),
                    error: None,
                }
            });
            let third = run_operations().start(test_request("dedupe-run-other"), async move {
                RunCapture {
                    history_entry: None,
                    result: None,
                    result_summary: Some("other".to_string()),
                    error: None,
                }
            });

            assert!(!first.reused_existing);
            assert!(second.reused_existing);
            assert_eq!(first.operation.operation_id, second.operation.operation_id);
            assert_ne!(first.operation.operation_id, third.operation.operation_id);

            tokio::time::sleep(Duration::from_millis(350)).await;
            let status = run_operations()
                .status_view(&first.operation.operation_id)
                .await
                .expect("status");
            assert_eq!(status.operation.status, "succeeded");
        });
    }

    #[test]
    fn run_operations_update_progress_while_running() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            run_operations().clear();
            let started = run_operations().start(test_request("progress-run"), async move {
                tokio::time::sleep(Duration::from_secs(3)).await;
                RunCapture {
                    history_entry: None,
                    result: None,
                    result_summary: Some("done".to_string()),
                    error: None,
                }
            });

            let initial = run_operations()
                .status_view(&started.operation.operation_id)
                .await
                .expect("initial status");
            assert_eq!(initial.operation.status, "running");
            let initial_update = initial.operation.last_update_at.clone();

            tokio::time::sleep(Duration::from_millis(1200)).await;
            let during = run_operations()
                .status_view(&started.operation.operation_id)
                .await
                .expect("running status");
            assert_eq!(during.operation.status, "running");
            assert_ne!(initial_update, during.operation.last_update_at);
            assert!(during.age_seconds >= initial.age_seconds);
            assert!(during.progress_percent >= 10);
            assert_eq!(during.operation.phase, "executing");

            tokio::time::sleep(Duration::from_secs(2)).await;
            let final_status = run_operations()
                .status_view(&started.operation.operation_id)
                .await
                .expect("terminal status");
            assert_eq!(final_status.operation.status, "succeeded");
            assert_eq!(final_status.progress_percent, 100);
        });
    }

    #[test]
    fn run_operations_apply_live_job_progress_events() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            run_operations().clear();
            let started = run_operations().start(test_request("live-progress"), async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                RunCapture {
                    history_entry: None,
                    result: None,
                    result_summary: Some("done".to_string()),
                    error: None,
                }
            });
            let operation_id = started.operation.operation_id.clone();

            run_operations().apply_progress_event(
                &operation_id,
                ExecutionProgressEvent::PlanPrepared {
                    run_id: "run-live".to_string(),
                    total_jobs: 2,
                },
            );
            run_operations().apply_progress_event(
                &operation_id,
                ExecutionProgressEvent::JobStarted {
                    name: "fetch-sources".to_string(),
                    stage: "deps".to_string(),
                },
            );
            let first = run_operations()
                .status_view(&operation_id)
                .await
                .expect("first progress");
            assert_eq!(first.operation.run_id.as_deref(), Some("run-live"));
            assert_eq!(first.operation.total_jobs, Some(2));
            assert_eq!(first.operation.active_job.as_deref(), Some("fetch-sources"));
            assert_eq!(first.operation.completed_jobs, 0);

            run_operations().apply_progress_event(
                &operation_id,
                ExecutionProgressEvent::JobFinished {
                    name: "fetch-sources".to_string(),
                    stage: "deps".to_string(),
                    status: ProgressJobStatus::Success,
                },
            );
            let second = run_operations()
                .status_view(&operation_id)
                .await
                .expect("second progress");
            assert_eq!(second.operation.completed_jobs, 1);
            assert_eq!(second.operation.failed_jobs, 0);
            assert!(second.operation.active_job.is_none());

            run_operations().apply_progress_event(
                &operation_id,
                ExecutionProgressEvent::JobFinished {
                    name: "rust-checks".to_string(),
                    stage: "test".to_string(),
                    status: ProgressJobStatus::Failed,
                },
            );
            let third = run_operations()
                .status_view(&operation_id)
                .await
                .expect("third progress");
            assert_eq!(third.operation.completed_jobs, 2);
            assert_eq!(third.operation.failed_jobs, 1);
        });
    }

    #[test]
    fn run_status_tool_exposes_progress_metadata() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            run_operations().clear();
            let started = run_operations().start(test_request("status-fields"), async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                RunCapture {
                    history_entry: None,
                    result: None,
                    result_summary: Some("done".to_string()),
                    error: None,
                }
            });

            let app = OpalApp::from_current_dir().expect("app");
            let status = call_tool(
                &app,
                json!({
                    "name": "opal_run_status",
                    "arguments": {
                        "operation_id": started.operation.operation_id
                    }
                }),
            )
            .await
            .expect("status");
            let operation = &status["structuredContent"]["operation"];
            assert!(operation["age_seconds"].as_i64().is_some());
            assert!(operation["last_update_age_seconds"].as_i64().is_some());
            assert!(operation["progress_percent"].as_u64().is_some());
            assert!(operation["is_stale"].is_boolean());
        });
    }

    #[test]
    fn operations_list_tool_discovers_running_operations_without_ids() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            run_operations().clear();
            let started = run_operations().start(test_request("ops-list-running"), async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                RunCapture {
                    history_entry: None,
                    result: None,
                    result_summary: Some("done".to_string()),
                    error: None,
                }
            });

            let app = OpalApp::from_current_dir().expect("app");
            let list = call_tool(
                &app,
                json!({
                    "name": "opal_operations_list",
                    "arguments": {
                        "active_only": true
                    }
                }),
            )
            .await
            .expect("operations list");

            assert_eq!(list["isError"], false);
            assert!(
                list["structuredContent"]["returned_operations"]
                    .as_u64()
                    .unwrap_or_default()
                    >= 1
            );
            let operations = list["structuredContent"]["operations"]
                .as_array()
                .expect("operations");
            assert!(operations.iter().any(|operation| {
                operation["operation_id"] == started.operation.operation_id
                    && operation["status"] == "running"
            }));
        });
    }

    #[test]
    fn run_operations_mark_stale_operations_failed() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            run_operations().clear();
            let started = run_operations().start(test_request("stale-run"), async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                RunCapture {
                    history_entry: None,
                    result: None,
                    result_summary: Some("done".to_string()),
                    error: None,
                }
            });
            let (timestamp, unix) = now_minus(120);
            run_operations().set_last_update_at_for_test(
                &started.operation.operation_id,
                &timestamp,
                unix,
            );

            let status = run_operations()
                .status_view(&started.operation.operation_id)
                .await
                .expect("status");
            assert_eq!(status.operation.status, "failed");
            assert_eq!(status.operation.phase, "completed");
            assert!(
                status
                    .operation
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("heartbeat became stale")
            );
        });
    }

    #[test]
    fn run_operations_convert_panics_to_failed_status() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            run_operations().clear();
            let started = run_operations().start(test_request("panic-run"), async move {
                panic!("forced panic for test");
            });
            tokio::time::sleep(Duration::from_millis(50)).await;

            let status = run_operations()
                .status_view(&started.operation.operation_id)
                .await
                .expect("status");
            assert_eq!(status.operation.status, "failed");
            assert_eq!(status.operation.phase, "completed");
            assert!(
                status
                    .operation
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("panicked")
            );
        });
    }

    #[test]
    fn run_status_tool_reports_background_failure() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let dir = tempdir().expect("tempdir");
            let opal_home = dir.path().join("opal-home-run-status");
            fs::create_dir_all(&opal_home).expect("opal home");
            unsafe {
                env::set_var("XDG_DATA_HOME", &opal_home);
            }
            run_operations().clear();
            fs::write(
                dir.path().join(".gitlab-ci.yml"),
                "stages:\n  - test\nhello:\n  stage: test\n  script:\n    - echo hello\n",
            )
            .expect("write pipeline");

            let app = OpalApp::from_current_dir().expect("app");
            let start = call_tool(
                &app,
                json!({
                    "name": "opal_run",
                    "arguments": {
                        "workdir": dir.path().display().to_string(),
                        "pipeline": "missing.yml"
                    }
                }),
            )
            .await
            .expect("start op");

            assert_eq!(start["isError"], false);
            let operation_id = start["structuredContent"]["operation"]["operation_id"]
                .as_str()
                .expect("operation id")
                .to_string();

            let mut terminal = None;
            for _ in 0..50 {
                let status = call_tool(
                    &app,
                    json!({
                        "name": "opal_run_status",
                        "arguments": {
                            "operation_id": operation_id
                        }
                    }),
                )
                .await
                .expect("status");
                let state = status["structuredContent"]["operation"]["status"]
                    .as_str()
                    .expect("status");
                if state != "running" {
                    terminal = Some(status);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }

            unsafe {
                env::remove_var("XDG_DATA_HOME");
            }

            let terminal = terminal.expect("terminal status");
            assert_eq!(
                terminal["structuredContent"]["operation"]["status"],
                "failed"
            );
            assert_eq!(
                terminal["structuredContent"]["operation"]["run"],
                Value::Null
            );
            assert!(
                !terminal["structuredContent"]["operation"]["error"]
                    .as_str()
                    .expect("error")
                    .is_empty()
            );
        });
    }

    #[test]
    fn view_tool_returns_selected_job_details_synchronously_without_log_reads() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-view-tool-metadata");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-1".to_string(),
                finished_at: "now".to_string(),
                status: HistoryStatus::Success,
                scope_root: Some(current_history_scope()),
                ref_name: None,
                pipeline_file: None,
                jobs: vec![HistoryJob {
                    name: "build".to_string(),
                    stage: "test".to_string(),
                    status: HistoryStatus::Success,
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

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_view",
                    "arguments": {
                        "run_id": "run-1",
                        "job": "build"
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["job"]["name"], "build");
        assert!(result["structuredContent"].get("job_log").is_none());
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn view_tool_uses_explicit_workdir_history_scope() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let workdir = dir.path().join("repo");
        let opal_home = dir.path().join("opal-home-view-explicit-workdir");
        fs::create_dir_all(&workdir).expect("workdir");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-explicit".to_string(),
                finished_at: "now".to_string(),
                status: HistoryStatus::Success,
                scope_root: Some(history_scope_root(&workdir)),
                ref_name: None,
                pipeline_file: None,
                jobs: vec![HistoryJob {
                    name: "build".to_string(),
                    stage: "test".to_string(),
                    status: HistoryStatus::Success,
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

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_view",
                    "arguments": {
                        "workdir": workdir.display().to_string(),
                        "run_id": "run-explicit",
                        "job": "build"
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["run"]["run_id"], "run-explicit");
        assert_eq!(result["structuredContent"]["job"]["name"], "build");
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn view_tool_returns_selected_job_details_via_background_operation() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-view-tool");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        let log_path = opal_home.join("job.log");
        fs::write(&log_path, "hello log").expect("write log");
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-1".to_string(),
                finished_at: "now".to_string(),
                status: HistoryStatus::Success,
                scope_root: Some(current_history_scope()),
                ref_name: None,
                pipeline_file: None,
                jobs: vec![HistoryJob {
                    name: "build".to_string(),
                    stage: "test".to_string(),
                    status: HistoryStatus::Success,
                    log_hash: "abc123".to_string(),
                    log_path: Some(log_path.display().to_string()),
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

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let start = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_view",
                    "arguments": {
                        "run_id": "run-1",
                        "job": "build",
                        "include_log": true
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(start["isError"], false);
        let operation_id = start["structuredContent"]["operation"]["operation_id"]
            .as_str()
            .expect("operation id");
        let result = wait_for_background_operation(&runtime, &app, operation_id);
        assert_eq!(
            result["structuredContent"]["operation"]["tool"],
            "opal_view"
        );
        assert_eq!(
            result["structuredContent"]["result"]["job"]["name"],
            "build"
        );
        assert_eq!(
            result["structuredContent"]["result"]["job_log"],
            "hello log"
        );
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn failed_jobs_tool_returns_latest_failed_jobs() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-failed-jobs-latest");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![HistoryJob {
                        name: "build".to_string(),
                        stage: "test".to_string(),
                        status: HistoryStatus::Success,
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
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "now".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![
                        HistoryJob {
                            name: "rust-checks".to_string(),
                            stage: "test".to_string(),
                            status: HistoryStatus::Failed,
                            log_hash: "def456".to_string(),
                            log_path: None,
                            artifact_dir: None,
                            artifacts: Vec::new(),
                            caches: Vec::new(),
                            container_name: None,
                            service_network: None,
                            service_containers: Vec::new(),
                            runtime_summary_path: None,
                            env_vars: Vec::new(),
                        },
                        HistoryJob {
                            name: "docs".to_string(),
                            stage: "test".to_string(),
                            status: HistoryStatus::Skipped,
                            log_hash: "ghi789".to_string(),
                            log_path: None,
                            artifact_dir: None,
                            artifacts: Vec::new(),
                            caches: Vec::new(),
                            container_name: None,
                            service_network: None,
                            service_containers: Vec::new(),
                            runtime_summary_path: None,
                            env_vars: Vec::new(),
                        },
                    ],
                },
            ],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_failed_jobs",
                    "arguments": {}
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["run"]["run_id"], "run-2");
        let failed = result["structuredContent"]["failed_jobs"]
            .as_array()
            .expect("failed jobs array");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["name"], "rust-checks");
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn failed_jobs_tool_honors_requested_run_id() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-failed-jobs-run-id");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![HistoryJob {
                        name: "lint".to_string(),
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
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "later".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![HistoryJob {
                        name: "build".to_string(),
                        stage: "test".to_string(),
                        status: HistoryStatus::Success,
                        log_hash: "def456".to_string(),
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
                },
            ],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_failed_jobs",
                    "arguments": {
                        "run_id": "run-1"
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["run"]["run_id"], "run-1");
        assert_eq!(result["structuredContent"]["failed_job_names"][0], "lint");
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn history_list_tool_filters_runs_by_status_and_limit() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-history-list-status");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "later".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-3".to_string(),
                    finished_at: "latest".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![],
                },
            ],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_history_list",
                    "arguments": {
                        "status": "failed",
                        "limit": 1
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["total_runs"], 3);
        assert_eq!(result["structuredContent"]["returned_runs"], 1);
        let runs = result["structuredContent"]["runs"]
            .as_array()
            .expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["run_id"], "run-3");
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn history_list_tool_uses_explicit_workdir_history_scope() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let workdir = dir.path().join("repo");
        let opal_home = dir.path().join("opal-home-history-list-workdir");
        fs::create_dir_all(&workdir).expect("workdir");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-explicit".to_string(),
                finished_at: "latest".to_string(),
                status: HistoryStatus::Success,
                scope_root: Some(history_scope_root(&workdir)),
                ref_name: None,
                pipeline_file: None,
                jobs: vec![],
            }],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_history_list",
                    "arguments": {
                        "workdir": workdir.display().to_string()
                    }
                }),
            ))
            .expect("call tool");

        let runs = result["structuredContent"]["runs"]
            .as_array()
            .expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["run_id"], "run-explicit");
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn history_list_tool_filters_runs_by_job_name() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-history-list-job");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![HistoryJob {
                        name: "build".to_string(),
                        stage: "test".to_string(),
                        status: HistoryStatus::Success,
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
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "latest".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![HistoryJob {
                        name: "rust-checks".to_string(),
                        stage: "test".to_string(),
                        status: HistoryStatus::Failed,
                        log_hash: "def456".to_string(),
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
                },
            ],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_history_list",
                    "arguments": {
                        "job": "rust-checks"
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        let runs = result["structuredContent"]["runs"]
            .as_array()
            .expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["run_id"], "run-2");
        assert_eq!(result["structuredContent"]["filters"]["job"], "rust-checks");
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn history_list_tool_filters_runs_by_date_range() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-history-list-date");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "2026-03-29T12:00:00Z".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "2026-03-30T12:00:00Z".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-3".to_string(),
                    finished_at: "2026-03-31T12:00:00Z".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![],
                },
            ],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_history_list",
                    "arguments": {
                        "since": "2026-03-30T00:00:00Z",
                        "until": "2026-03-30T23:59:59Z"
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        let runs = result["structuredContent"]["runs"]
            .as_array()
            .expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["run_id"], "run-2");
        assert_eq!(
            result["structuredContent"]["filters"]["since"],
            "2026-03-30T00:00:00Z"
        );
        assert_eq!(
            result["structuredContent"]["filters"]["until"],
            "2026-03-30T23:59:59Z"
        );
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn history_list_tool_filters_runs_by_branch_and_pipeline_file() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-history-list-branch-pipeline");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "2026-03-29T12:00:00Z".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: Some("main".to_string()),
                    pipeline_file: Some(".gitlab-ci.yml".to_string()),
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "2026-03-30T12:00:00Z".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: Some("release".to_string()),
                    pipeline_file: Some(".gitlab-ci.yml".to_string()),
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-3".to_string(),
                    finished_at: "2026-03-31T12:00:00Z".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: Some("main".to_string()),
                    pipeline_file: Some("pipelines/docs.yml".to_string()),
                    jobs: vec![],
                },
            ],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_history_list",
                    "arguments": {
                        "branch": "main",
                        "pipeline_file": "pipelines/docs.yml"
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        let runs = result["structuredContent"]["runs"]
            .as_array()
            .expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["run_id"], "run-3");
        assert_eq!(result["structuredContent"]["filters"]["branch"], "main");
        assert_eq!(
            result["structuredContent"]["filters"]["pipeline_file"],
            "pipelines/docs.yml"
        );
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn history_list_tool_ignores_runs_from_other_scopes() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-history-list-scope");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-local".to_string(),
                    finished_at: "2026-03-30T12:00:00Z".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: Some("main".to_string()),
                    pipeline_file: Some(".gitlab-ci.yml".to_string()),
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-foreign".to_string(),
                    finished_at: "2026-03-31T12:00:00Z".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some("/tmp/other-repo".to_string()),
                    ref_name: Some("main".to_string()),
                    pipeline_file: Some(".gitlab-ci.yml".to_string()),
                    jobs: vec![],
                },
            ],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_history_list",
                    "arguments": {}
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["total_runs"], 1);
        let runs = result["structuredContent"]["runs"]
            .as_array()
            .expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["run_id"], "run-local");
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn run_diff_tool_compares_latest_two_runs() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-run-diff-latest");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![
                        HistoryJob {
                            name: "build".to_string(),
                            stage: "build".to_string(),
                            status: HistoryStatus::Success,
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
                        },
                        HistoryJob {
                            name: "docs".to_string(),
                            stage: "test".to_string(),
                            status: HistoryStatus::Failed,
                            log_hash: "def456".to_string(),
                            log_path: None,
                            artifact_dir: None,
                            artifacts: Vec::new(),
                            caches: Vec::new(),
                            container_name: None,
                            service_network: None,
                            service_containers: Vec::new(),
                            runtime_summary_path: None,
                            env_vars: Vec::new(),
                        },
                    ],
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "later".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![
                        HistoryJob {
                            name: "build".to_string(),
                            stage: "build".to_string(),
                            status: HistoryStatus::Success,
                            log_hash: "ghi789".to_string(),
                            log_path: None,
                            artifact_dir: None,
                            artifacts: Vec::new(),
                            caches: Vec::new(),
                            container_name: None,
                            service_network: None,
                            service_containers: Vec::new(),
                            runtime_summary_path: None,
                            env_vars: Vec::new(),
                        },
                        HistoryJob {
                            name: "docs".to_string(),
                            stage: "docs".to_string(),
                            status: HistoryStatus::Success,
                            log_hash: "jkl012".to_string(),
                            log_path: None,
                            artifact_dir: None,
                            artifacts: Vec::new(),
                            caches: Vec::new(),
                            container_name: None,
                            service_network: None,
                            service_containers: Vec::new(),
                            runtime_summary_path: None,
                            env_vars: Vec::new(),
                        },
                        HistoryJob {
                            name: "lint".to_string(),
                            stage: "test".to_string(),
                            status: HistoryStatus::Skipped,
                            log_hash: "mno345".to_string(),
                            log_path: None,
                            artifact_dir: None,
                            artifacts: Vec::new(),
                            caches: Vec::new(),
                            container_name: None,
                            service_network: None,
                            service_containers: Vec::new(),
                            runtime_summary_path: None,
                            env_vars: Vec::new(),
                        },
                    ],
                },
            ],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_run_diff",
                    "arguments": {}
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["base_run"]["run_id"], "run-1");
        assert_eq!(result["structuredContent"]["head_run"]["run_id"], "run-2");
        assert_eq!(result["structuredContent"]["overall_status_changed"], true);
        assert_eq!(
            result["structuredContent"]["changed_jobs"][0]["name"],
            "docs"
        );
        assert_eq!(result["structuredContent"]["added_jobs"][0]["name"], "lint");
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn run_diff_tool_honors_explicit_base_run_id() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-run-diff-explicit");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "first".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![HistoryJob {
                        name: "build".to_string(),
                        stage: "build".to_string(),
                        status: HistoryStatus::Success,
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
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "second".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![HistoryJob {
                        name: "build".to_string(),
                        stage: "build".to_string(),
                        status: HistoryStatus::Failed,
                        log_hash: "def456".to_string(),
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
                },
                HistoryEntry {
                    run_id: "run-3".to_string(),
                    finished_at: "third".to_string(),
                    status: HistoryStatus::Success,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![HistoryJob {
                        name: "build".to_string(),
                        stage: "build".to_string(),
                        status: HistoryStatus::Success,
                        log_hash: "ghi789".to_string(),
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
                },
            ],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_run_diff",
                    "arguments": {
                        "run_id": "run-3",
                        "base_run_id": "run-1"
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["base_run"]["run_id"], "run-1");
        assert_eq!(result["structuredContent"]["head_run"]["run_id"], "run-3");
        assert_eq!(result["structuredContent"]["overall_status_changed"], false);
        let changed = result["structuredContent"]["changed_jobs"]
            .as_array()
            .expect("changed jobs");
        assert!(changed.is_empty());
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn logs_search_tool_finds_matches_across_runs() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-logs-search");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        let build_log = opal_home.join("build.log");
        let docs_log = opal_home.join("docs.log");
        fs::write(&build_log, "all good\nwarning: retry later\n").expect("write build log");
        fs::write(&docs_log, "fatal: dependency missing\nfatal: docs failed\n")
            .expect("write docs log");
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![HistoryJob {
                        name: "build".to_string(),
                        stage: "build".to_string(),
                        status: HistoryStatus::Success,
                        log_hash: "abc123".to_string(),
                        log_path: Some(build_log.display().to_string()),
                        artifact_dir: None,
                        artifacts: Vec::new(),
                        caches: Vec::new(),
                        container_name: None,
                        service_network: None,
                        service_containers: Vec::new(),
                        runtime_summary_path: None,
                        env_vars: Vec::new(),
                    }],
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "latest".to_string(),
                    status: HistoryStatus::Failed,
                    scope_root: Some(current_history_scope()),
                    ref_name: None,
                    pipeline_file: None,
                    jobs: vec![HistoryJob {
                        name: "docs".to_string(),
                        stage: "test".to_string(),
                        status: HistoryStatus::Failed,
                        log_hash: "def456".to_string(),
                        log_path: Some(docs_log.display().to_string()),
                        artifact_dir: None,
                        artifacts: Vec::new(),
                        caches: Vec::new(),
                        container_name: None,
                        service_network: None,
                        service_containers: Vec::new(),
                        runtime_summary_path: None,
                        env_vars: Vec::new(),
                    }],
                },
            ],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let start = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_logs_search",
                    "arguments": {
                        "query": "fatal"
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(start["isError"], false);
        let operation_id = start["structuredContent"]["operation"]["operation_id"]
            .as_str()
            .expect("operation id");
        let result = wait_for_background_operation(&runtime, &app, operation_id);
        assert_eq!(
            result["structuredContent"]["operation"]["tool"],
            "opal_logs_search"
        );
        assert_eq!(result["structuredContent"]["result"]["returned_matches"], 1);
        assert_eq!(
            result["structuredContent"]["result"]["matches"][0]["run_id"],
            "run-2"
        );
        assert_eq!(
            result["structuredContent"]["result"]["matches"][0]["job"],
            "docs"
        );
        assert_eq!(
            result["structuredContent"]["result"]["matches"][0]["line_matches"],
            2
        );
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn logs_search_tool_honors_job_and_case_filters() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-logs-search-filters");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        let build_log = opal_home.join("build.log");
        let docs_log = opal_home.join("docs.log");
        fs::write(&build_log, "Fatal build issue\n").expect("write build log");
        fs::write(&docs_log, "fatal docs issue\n").expect("write docs log");
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-1".to_string(),
                finished_at: "now".to_string(),
                status: HistoryStatus::Failed,
                scope_root: Some(current_history_scope()),
                ref_name: None,
                pipeline_file: None,
                jobs: vec![
                    HistoryJob {
                        name: "build".to_string(),
                        stage: "build".to_string(),
                        status: HistoryStatus::Failed,
                        log_hash: "abc123".to_string(),
                        log_path: Some(build_log.display().to_string()),
                        artifact_dir: None,
                        artifacts: Vec::new(),
                        caches: Vec::new(),
                        container_name: None,
                        service_network: None,
                        service_containers: Vec::new(),
                        runtime_summary_path: None,
                        env_vars: Vec::new(),
                    },
                    HistoryJob {
                        name: "docs".to_string(),
                        stage: "test".to_string(),
                        status: HistoryStatus::Failed,
                        log_hash: "def456".to_string(),
                        log_path: Some(docs_log.display().to_string()),
                        artifact_dir: None,
                        artifacts: Vec::new(),
                        caches: Vec::new(),
                        container_name: None,
                        service_network: None,
                        service_containers: Vec::new(),
                        runtime_summary_path: None,
                        env_vars: Vec::new(),
                    },
                ],
            }],
        )
        .expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let start = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_logs_search",
                    "arguments": {
                        "query": "Fatal",
                        "job": "build",
                        "case_sensitive": true
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(start["isError"], false);
        let operation_id = start["structuredContent"]["operation"]["operation_id"]
            .as_str()
            .expect("operation id");
        let result = wait_for_background_operation(&runtime, &app, operation_id);
        assert_eq!(result["structuredContent"]["result"]["returned_matches"], 1);
        assert_eq!(
            result["structuredContent"]["result"]["matches"][0]["job"],
            "build"
        );
        assert_eq!(
            result["structuredContent"]["result"]["matches"][0]["matching_lines"][0]["text"],
            "Fatal build issue"
        );
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn logs_search_tool_uses_explicit_workdir_history_scope() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let workdir = dir.path().join("repo");
        let opal_home = dir.path().join("opal-home-logs-search-workdir");
        fs::create_dir_all(&workdir).expect("workdir");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        let log_path = opal_home.join("build.log");
        fs::write(&log_path, "fatal explicit workdir issue\n").expect("write log");
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-explicit".to_string(),
                finished_at: "now".to_string(),
                status: HistoryStatus::Failed,
                scope_root: Some(history_scope_root(&workdir)),
                ref_name: None,
                pipeline_file: None,
                jobs: vec![HistoryJob {
                    name: "build".to_string(),
                    stage: "test".to_string(),
                    status: HistoryStatus::Failed,
                    log_hash: "abc123".to_string(),
                    log_path: Some(log_path.display().to_string()),
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

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let start = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_logs_search",
                    "arguments": {
                        "workdir": workdir.display().to_string(),
                        "query": "explicit workdir"
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(start["isError"], false);
        let operation_id = start["structuredContent"]["operation"]["operation_id"]
            .as_str()
            .expect("operation id");
        let result = wait_for_background_operation(&runtime, &app, operation_id);
        assert_eq!(result["structuredContent"]["result"]["returned_matches"], 1);
        assert_eq!(
            result["structuredContent"]["result"]["matches"][0]["run_id"],
            "run-explicit"
        );
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn job_rerun_request_uses_recorded_job_name() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-job-rerun-request");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-1".to_string(),
                finished_at: "2026-03-31T12:00:00Z".to_string(),
                status: HistoryStatus::Failed,
                scope_root: Some(current_history_scope()),
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

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let request = runtime
            .block_on(job_rerun_request(
                &app,
                &json!({
                    "job": "rust-checks",
                    "engine": "docker"
                }),
            ))
            .expect("job rerun request");

        assert_eq!(request.source_run.run_id, "run-1");
        assert_eq!(request.source_job.name, "rust-checks");
        assert_eq!(request.run_args.jobs, vec!["rust-checks"]);
        assert_eq!(request.run_args.engine, crate::EngineChoice::Docker);
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn job_rerun_request_uses_explicit_workdir_history_scope() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let workdir = dir.path().join("repo");
        let opal_home = dir.path().join("opal-home-job-rerun-explicit-workdir");
        fs::create_dir_all(&workdir).expect("workdir");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-explicit".to_string(),
                finished_at: "2026-03-31T12:00:00Z".to_string(),
                status: HistoryStatus::Failed,
                scope_root: Some(history_scope_root(&workdir)),
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

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let request = runtime
            .block_on(job_rerun_request(
                &app,
                &json!({
                    "workdir": workdir.display().to_string(),
                    "run_id": "run-explicit",
                    "job": "rust-checks"
                }),
            ))
            .expect("request");

        assert_eq!(request.source_run.run_id, "run-explicit");
        assert_eq!(request.source_job.name, "rust-checks");
        assert_eq!(request.run_args.workdir, Some(workdir));
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn plan_explain_tool_reports_selected_dependency_closure() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-plan-explain-selected-closure");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        fs::write(
            dir.path().join(".gitlab-ci.yml"),
            concat!(
                "stages: [build, test]\n",
                "build:\n",
                "  stage: build\n",
                "  script: [\"echo build\"]\n",
                "test:\n",
                "  stage: test\n",
                "  needs: [build]\n",
                "  script: [\"echo test\"]\n"
            ),
        )
        .expect("write pipeline");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_plan_explain",
                    "arguments": {
                        "workdir": dir.path().display().to_string(),
                        "job": "build",
                        "jobs": ["test"]
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["job"]["status"], "included");
        assert_eq!(result["structuredContent"]["job"]["selected"], true);
        assert_eq!(
            result["structuredContent"]["job"]["selected_directly"],
            false
        );
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn plan_explain_tool_reports_blocked_jobs_outside_selected_slice() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-plan-explain-blocked");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        fs::write(
            dir.path().join(".gitlab-ci.yml"),
            concat!(
                "stages: [build, docs]\n",
                "build:\n",
                "  stage: build\n",
                "  script: [\"echo build\"]\n",
                "docs:\n",
                "  stage: docs\n",
                "  script: [\"echo docs\"]\n"
            ),
        )
        .expect("write pipeline");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_plan_explain",
                    "arguments": {
                        "workdir": dir.path().display().to_string(),
                        "job": "docs",
                        "jobs": ["build"]
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["job"]["status"], "blocked");
        assert_eq!(result["structuredContent"]["job"]["selected"], false);
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn plan_explain_tool_reports_skipped_jobs() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-plan-explain-skipped");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        fs::write(
            dir.path().join(".gitlab-ci.yml"),
            concat!(
                "stages: [test]\n",
                "never-job:\n",
                "  stage: test\n",
                "  when: never\n",
                "  script: [\"echo nope\"]\n"
            ),
        )
        .expect("write pipeline");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_plan_explain",
                    "arguments": {
                        "workdir": dir.path().display().to_string(),
                        "job": "never-job"
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["job"]["status"], "skipped");
        assert_eq!(
            result["structuredContent"]["job"]["resolved_name"],
            "never-job"
        );
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn engine_status_tool_reports_configured_default_and_available_binaries() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let workdir = dir.path().join("repo");
        let opal_home = dir.path().join("opal-home-engine-status");
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(workdir.join(".opal")).expect("repo config dir");
        fs::create_dir_all(&opal_home).expect("opal home");
        fs::create_dir_all(&bin_dir).expect("bin dir");
        fs::write(
            workdir.join(".opal").join("config.toml"),
            "[engine]\ndefault = \"docker\"\n",
        )
        .expect("write config");
        fs::write(bin_dir.join("docker"), "").expect("docker binary");
        fs::write(bin_dir.join("podman"), "").expect("podman binary");

        let original_path: Option<OsString> = env::var_os("PATH");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
            env::set_var("PATH", &bin_dir);
        }

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let result = runtime
            .block_on(call_tool(
                &app,
                json!({
                    "name": "opal_engine_status",
                    "arguments": {
                        "workdir": workdir.display().to_string()
                    }
                }),
            ))
            .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["configured_default"], "docker");
        let engines = result["structuredContent"]["engines"]
            .as_array()
            .expect("engines array");
        let docker = engines
            .iter()
            .find(|engine| engine["choice"] == "docker")
            .expect("docker entry");
        let container = engines
            .iter()
            .find(|engine| engine["choice"] == "container")
            .expect("container entry");
        assert_eq!(docker["available"], true);
        assert_eq!(container["available"], false);

        if let Some(path) = original_path {
            unsafe {
                env::set_var("PATH", path);
            }
        } else {
            unsafe {
                env::remove_var("PATH");
            }
        }
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }
}
