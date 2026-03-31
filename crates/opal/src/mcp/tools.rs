use crate::app::OpalApp;
use crate::app::plan::render as render_plan;
use crate::app::run::execute_and_capture;
use crate::app::view::{
    find_history_entry, find_job, latest_history_entry, read_job_log, read_runtime_summary,
};
use crate::history::HistoryEntry;
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

    let entry = match run_id {
        Some(run_id) => find_history_entry(run_id)?
            .with_context(|| format!("run '{run_id}' not found in Opal history"))?,
        None => latest_history_entry()?.context("no Opal history entries found")?,
    };

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

    #[tokio::test]
    async fn view_tool_returns_selected_job_details() {
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
        let result = call_tool(
            &app,
            json!({
                "name": "opal_view",
                "arguments": {
                    "run_id": "run-1",
                    "job": "build",
                    "include_log": true
                }
            }),
        )
        .await
        .expect("call tool");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["job"]["name"], "build");
        assert_eq!(result["structuredContent"]["job_log"], "hello log");
        unsafe {
            env::remove_var("OPAL_HOME");
        }
    }
}
