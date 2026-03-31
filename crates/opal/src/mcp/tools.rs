use crate::app::OpalApp;
use crate::app::plan::render as render_plan;
use crate::app::run::execute_and_capture;
use crate::app::view::{
    find_history_entry, find_job, latest_history_entry, load_history, read_job_log,
    read_runtime_summary,
};
use crate::history::{HistoryEntry, HistoryStatus};
use crate::{EngineChoice, PlanArgs, RunArgs, ViewArgs};
use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
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
                        "limit": { "type": "integer", "minimum": 1 }
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
        .take(limit)
        .collect::<Vec<_>>();

    let filter_summary = history_filter_summary(status, job_name, limit);
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
                "limit": limit,
            },
            "total_runs": total_runs,
            "returned_runs": runs.len(),
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
    limit: usize,
) -> String {
    let mut filters = Vec::new();
    if let Some(status) = status {
        filters.push(format!("status={}", history_status_label(status)));
    }
    if let Some(job_name) = job_name {
        filters.push(format!("job={job_name}"));
    }
    filters.push(format!("limit={limit}"));
    filters.join(", ")
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
    use super::{call_tool, list_tools, run_args_from_value};
    use crate::app::OpalApp;
    use crate::history::{HistoryEntry, HistoryJob, HistoryStatus, save};
    use crate::mcp::TEST_ENV_LOCK;
    use crate::runtime;
    use serde_json::json;
    use std::env;
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
}
