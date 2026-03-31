use crate::app::OpalApp;
use crate::app::plan::{explain as explain_plan, render as render_plan};
use crate::app::run::execute_and_capture;
use crate::app::view::{
    find_history_entry, find_job, latest_history_entry, load_history, read_job_log,
    read_runtime_summary,
};
use crate::config::OpalConfig;
use crate::history::{HistoryEntry, HistoryStatus};
use crate::{EngineChoice, EngineKind, PlanArgs, RunArgs, ViewArgs};
use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};
use std::str::FromStr;

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
                "description": "Runs the local pipeline without the TUI and returns the latest recorded run summary.",
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
                "name": "opal_view",
                "title": "Inspect Opal history and logs",
                "description": "Returns the latest or selected recorded Opal run, with optional job log and runtime summary.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
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
                        "run_id": { "type": "string" }
                    }
                }
            },
            {
                "name": "opal_history_list",
                "title": "List recorded Opal runs",
                "description": "Returns recorded Opal runs with optional status and job-name filters.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "enum": ["success", "failed", "skipped", "running"]
                        },
                        "job": { "type": "string" },
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
                        "run_id": { "type": "string" },
                        "base_run_id": { "type": "string" }
                    }
                }
            },
            {
                "name": "opal_logs_search",
                "title": "Search recorded Opal job logs",
                "description": "Searches recorded Opal job logs for recurring failures or exact strings.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
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
            let rendered = render_plan(app, plan_args_from_value(arguments)?)?;
            Ok(tool_result(
                rendered.content,
                json!({
                    "json": rendered.json
                }),
                false,
            ))
        }
        "opal_run" => Ok(run_tool(app, arguments).await),
        "opal_view" => Ok(view_tool(arguments)?),
        "opal_failed_jobs" => Ok(failed_jobs_tool(arguments)?),
        "opal_history_list" => Ok(history_list_tool(arguments)?),
        "opal_run_diff" => Ok(run_diff_tool(arguments)?),
        "opal_logs_search" => Ok(logs_search_tool(arguments)?),
        "opal_job_rerun" => Ok(job_rerun_tool(app, arguments).await),
        "opal_plan_explain" => Ok(plan_explain_tool(app, arguments)?),
        "opal_engine_status" => Ok(engine_status_tool(app, arguments)?),
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
    let capture = execute_and_capture(app, args).await;
    let structured = capture
        .history_entry
        .as_ref()
        .map(history_entry_json)
        .unwrap_or(Value::Null);
    let text = match (&capture.history_entry, &capture.error) {
        (Some(entry), Some(err)) => {
            format!(
                "Opal run {} finished with status {:?}: {err}",
                entry.run_id, entry.status
            )
        }
        (Some(entry), None) => {
            format!(
                "Opal run {} finished with status {:?}",
                entry.run_id, entry.status
            )
        }
        (None, Some(err)) => format!("Opal run failed before recording history: {err}"),
        (None, None) => "Opal run completed without a recorded history entry".to_string(),
    };
    tool_result(text, structured, capture.error.is_some())
}

fn view_tool(arguments: Value) -> Result<Value> {
    let run_id = arguments.get("run_id").and_then(Value::as_str);
    let job_name = arguments.get("job").and_then(Value::as_str);
    let include_log = arguments
        .get("include_log")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_runtime_summary = arguments
        .get("include_runtime_summary")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let entry = selected_history_entry(run_id)?;

    let mut structured = Map::new();
    structured.insert("run".to_string(), history_entry_json(&entry));

    let mut text = format!(
        "Opal run {} finished at {} with status {:?}",
        entry.run_id, entry.finished_at, entry.status
    );

    if let Some(job_name) = job_name {
        let job = find_job(&entry, job_name)
            .with_context(|| format!("job '{job_name}' not found in run '{}'", entry.run_id))?;
        structured.insert("job".to_string(), json!(job));
        text.push_str(&format!("\nSelected job: {} ({:?})", job.name, job.status));
        if include_log {
            let log = read_job_log(&entry, job)?;
            structured.insert("job_log".to_string(), json!(log));
            text.push_str("\nIncluded job log.");
        }
        if include_runtime_summary {
            let summary = read_runtime_summary(job)?;
            structured.insert("runtime_summary".to_string(), json!(summary));
            text.push_str("\nIncluded runtime summary.");
        }
    } else {
        text.push_str(&format!("\nJobs recorded: {}", entry.jobs.len()));
    }

    Ok(tool_result(text, Value::Object(structured), false))
}

fn failed_jobs_tool(arguments: Value) -> Result<Value> {
    let run_id = arguments.get("run_id").and_then(Value::as_str);
    let entry = selected_history_entry(run_id)?;
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

fn history_list_tool(arguments: Value) -> Result<Value> {
    let status = history_status_from_value(arguments.get("status"))?;
    let job_name = arguments.get("job").and_then(Value::as_str);
    let since = history_time_filter(arguments.get("since"), "since")?;
    let until = history_time_filter(arguments.get("until"), "until")?;
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(20);

    let history = load_history()?;
    let total_runs = history.len();
    let runs = history
        .into_iter()
        .rev()
        .filter(|entry| matches_status(entry, status))
        .filter(|entry| matches_job_name(entry, job_name))
        .filter(|entry| matches_finished_at(entry, since.as_deref(), until.as_deref()))
        .take(limit)
        .collect::<Vec<_>>();

    let filter_summary =
        history_filter_summary(status, job_name, since.as_deref(), until.as_deref(), limit);
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

fn run_diff_tool(arguments: Value) -> Result<Value> {
    let run_id = arguments.get("run_id").and_then(Value::as_str);
    let base_run_id = arguments.get("base_run_id").and_then(Value::as_str);
    let history = load_history()?;
    let (base_run, head_run) = selected_run_pair(&history, run_id, base_run_id)?;
    let diff = compare_runs(&base_run, &head_run);
    Ok(tool_result(
        render_run_diff_summary(&diff),
        json!(diff),
        false,
    ))
}

fn logs_search_tool(arguments: Value) -> Result<Value> {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .context("missing query")?;
    let run_id = arguments.get("run_id").and_then(Value::as_str);
    let job_name = arguments.get("job").and_then(Value::as_str);
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(20);
    let case_sensitive = arguments
        .get("case_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let history = load_history()?;
    let mut matches = Vec::new();
    let mut read_errors = Vec::new();
    let mut scanned_jobs = 0usize;

    for entry in history.iter().rev() {
        if run_id.is_some_and(|run_id| entry.run_id != run_id) {
            continue;
        }
        for job in &entry.jobs {
            if job_name.is_some_and(|job_name| job.name != job_name) {
                continue;
            }
            scanned_jobs += 1;
            match read_job_log(entry, job) {
                Ok(log) => {
                    if let Some(log_match) = find_log_match(entry, job, &log, query, case_sensitive)
                    {
                        matches.push(log_match);
                        if matches.len() >= limit {
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
        if matches.len() >= limit {
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

    Ok(tool_result(
        text,
        json!({
            "query": query,
            "case_sensitive": case_sensitive,
            "filters": {
                "run_id": run_id,
                "job": job_name,
                "limit": limit,
            },
            "matches": matches,
            "returned_matches": matches.len(),
            "scanned_jobs": scanned_jobs,
            "read_errors": read_errors,
        }),
        false,
    ))
}

async fn job_rerun_tool(app: &OpalApp, arguments: Value) -> Value {
    let request = match job_rerun_request(&arguments) {
        Ok(request) => request,
        Err(err) => return error_tool_result(err.to_string(), Value::Null),
    };
    let capture = execute_and_capture(app, request.run_args).await;
    let structured = json!({
        "source_run": history_entry_json(&request.source_run),
        "source_job": request.source_job,
        "requested_job": request.requested_job,
        "rerun": capture.history_entry.as_ref().map(history_entry_json).unwrap_or(Value::Null),
    });
    let text = match (&capture.history_entry, &capture.error) {
        (Some(entry), Some(err)) => format!(
            "Reran job {} from recorded run {} as Opal run {} with status {:?}: {err}",
            request.requested_job, request.source_run.run_id, entry.run_id, entry.status
        ),
        (Some(entry), None) => format!(
            "Reran job {} from recorded run {} as Opal run {} with status {:?}",
            request.requested_job, request.source_run.run_id, entry.run_id, entry.status
        ),
        (None, Some(err)) => format!(
            "Failed to rerun job {} from recorded run {}: {err}",
            request.requested_job, request.source_run.run_id
        ),
        (None, None) => format!(
            "Reran job {} from recorded run {} without a recorded history entry",
            request.requested_job, request.source_run.run_id
        ),
    };
    tool_result(text, structured, capture.error.is_some())
}

fn plan_explain_tool(app: &OpalApp, arguments: Value) -> Result<Value> {
    let job = arguments
        .get("job")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .context("missing job")?;
    let explanation = explain_plan(app, plan_args_from_value(arguments)?, &job)?;
    Ok(tool_result(
        explanation.summary,
        json!(explanation.details),
        false,
    ))
}

fn engine_status_tool(app: &OpalApp, arguments: Value) -> Result<Value> {
    let workdir = app.resolve_workdir(
        arguments
            .get("workdir")
            .and_then(Value::as_str)
            .map(PathBuf::from),
    );
    let settings = OpalConfig::load(&workdir)?;
    let configured_default = settings.default_engine().unwrap_or(EngineChoice::Auto);
    let resolved_auto = resolved_engine_choice(EngineChoice::Auto, &settings);
    let engines = [
        EngineChoice::Container,
        EngineChoice::Docker,
        EngineChoice::Podman,
        EngineChoice::Nerdctl,
        EngineChoice::Orbstack,
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

fn history_entry_json(entry: &HistoryEntry) -> Value {
    json!(entry)
}

fn selected_history_entry(run_id: Option<&str>) -> Result<HistoryEntry> {
    match run_id {
        Some(run_id) => find_history_entry(run_id)?
            .with_context(|| format!("run '{run_id}' not found in Opal history")),
        None => latest_history_entry()?.context("no Opal history entries found"),
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

fn history_filter_summary(
    status: Option<HistoryStatus>,
    job_name: Option<&str>,
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

fn job_rerun_request(arguments: &Value) -> Result<JobRerunRequest> {
    let run_id = arguments.get("run_id").and_then(Value::as_str);
    let requested_job = arguments
        .get("job")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .context("missing job")?;
    let source_run = selected_history_entry(run_id)?;
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
        }
    }

    #[cfg(target_os = "linux")]
    {
        match selected {
            EngineChoice::Auto | EngineChoice::Podman => EngineKind::Podman,
            EngineChoice::Docker => EngineKind::Docker,
            EngineChoice::Nerdctl => EngineKind::Nerdctl,
            EngineChoice::Orbstack | EngineChoice::Container => EngineKind::Docker,
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
        return (true, orbstack_or_container_note(choice));
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
            _ => (true, orbstack_or_container_note(choice)),
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let note = Some("Opal falls back to docker on this platform".to_string());
        let _ = choice;
        (true, note)
    }
}

fn orbstack_or_container_note(choice: EngineChoice) -> Option<String> {
    match choice {
        EngineChoice::Orbstack => Some(
            "Orbstack uses the docker CLI; availability does not distinguish backend identity"
                .to_string(),
        ),
        EngineChoice::Container => {
            Some("Container runtime uses the Apple container CLI".to_string())
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
    }
}

fn engine_kind_name(kind: EngineKind) -> &'static str {
    match kind {
        EngineKind::ContainerCli => "container",
        EngineKind::Docker => "docker",
        EngineKind::Podman => "podman",
        EngineKind::Nerdctl => "nerdctl",
        EngineKind::Orbstack => "orbstack",
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
    use super::{call_tool, job_rerun_request, list_tools, run_args_from_value};
    use crate::app::OpalApp;
    use crate::history::{HistoryEntry, HistoryJob, HistoryStatus, save};
    use crate::mcp::TEST_ENV_LOCK;
    use crate::runtime;
    use serde_json::json;
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn tools_list_exposes_run_plan_and_view() {
        let response = list_tools();
        let tools = response["tools"].as_array().expect("tool array");
        assert!(tools.iter().any(|tool| tool["name"] == "opal_plan"));
        assert!(tools.iter().any(|tool| tool["name"] == "opal_run"));
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
    fn view_tool_returns_selected_job_details() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-view-tool");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        let log_path = opal_home.join("job.log");
        fs::write(&log_path, "hello log").expect("write log");
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-1".to_string(),
                finished_at: "now".to_string(),
                status: HistoryStatus::Success,
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
        let result = runtime
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

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["job"]["name"], "build");
        assert_eq!(result["structuredContent"]["job_log"], "hello log");
        unsafe {
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn failed_jobs_tool_returns_latest_failed_jobs() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-failed-jobs-latest");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Success,
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
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn failed_jobs_tool_honors_requested_run_id() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-failed-jobs-run-id");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Failed,
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
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn history_list_tool_filters_runs_by_status_and_limit() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-history-list-status");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Success,
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "later".to_string(),
                    status: HistoryStatus::Failed,
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-3".to_string(),
                    finished_at: "latest".to_string(),
                    status: HistoryStatus::Failed,
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
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn history_list_tool_filters_runs_by_job_name() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-history-list-job");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Success,
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
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn history_list_tool_filters_runs_by_date_range() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-history-list-date");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "2026-03-29T12:00:00Z".to_string(),
                    status: HistoryStatus::Success,
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-2".to_string(),
                    finished_at: "2026-03-30T12:00:00Z".to_string(),
                    status: HistoryStatus::Failed,
                    jobs: vec![],
                },
                HistoryEntry {
                    run_id: "run-3".to_string(),
                    finished_at: "2026-03-31T12:00:00Z".to_string(),
                    status: HistoryStatus::Success,
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
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn run_diff_tool_compares_latest_two_runs() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-run-diff-latest");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "earlier".to_string(),
                    status: HistoryStatus::Failed,
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
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn run_diff_tool_honors_explicit_base_run_id() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-run-diff-explicit");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[
                HistoryEntry {
                    run_id: "run-1".to_string(),
                    finished_at: "first".to_string(),
                    status: HistoryStatus::Success,
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
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn logs_search_tool_finds_matches_across_runs() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-logs-search");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
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
        let result = runtime
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

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["returned_matches"], 1);
        assert_eq!(result["structuredContent"]["matches"][0]["run_id"], "run-2");
        assert_eq!(result["structuredContent"]["matches"][0]["job"], "docs");
        assert_eq!(result["structuredContent"]["matches"][0]["line_matches"], 2);
        unsafe {
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn logs_search_tool_honors_job_and_case_filters() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-logs-search-filters");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
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
        let result = runtime
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

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["returned_matches"], 1);
        assert_eq!(result["structuredContent"]["matches"][0]["job"], "build");
        assert_eq!(
            result["structuredContent"]["matches"][0]["matching_lines"][0]["text"],
            "Fatal build issue"
        );
        unsafe {
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn job_rerun_request_uses_recorded_job_name() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-job-rerun-request");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-1".to_string(),
                finished_at: "2026-03-31T12:00:00Z".to_string(),
                status: HistoryStatus::Failed,
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

        let request = job_rerun_request(&json!({
            "job": "rust-checks",
            "engine": "docker"
        }))
        .expect("job rerun request");

        assert_eq!(request.source_run.run_id, "run-1");
        assert_eq!(request.source_job.name, "rust-checks");
        assert_eq!(request.run_args.jobs, vec!["rust-checks"]);
        assert_eq!(request.run_args.engine, crate::EngineChoice::Docker);
        unsafe {
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn plan_explain_tool_reports_selected_dependency_closure() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
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
    }

    #[test]
    fn plan_explain_tool_reports_blocked_jobs_outside_selected_slice() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
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
    }

    #[test]
    fn plan_explain_tool_reports_skipped_jobs() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
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
            env::set_var("OPAL_HOME", &opal_home);
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
            env::remove_var("OPAL_HOME");
        }
    }
}
