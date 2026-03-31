use crate::app::view::{
    find_history_entry, find_job, latest_history_entry, load_history, read_job_log,
    read_runtime_summary,
};
use crate::history::HistoryEntry;
use crate::mcp::uri::{ResourceUri, encode_path_segment, parse_resource_uri};
use anyhow::{Context, Result};
use serde_json::{Value, json};

pub(crate) fn list_resources() -> Result<Value> {
    let history = load_history()?;
    let mut resources = Vec::new();
    resources.push(json!({
        "uri": "opal://history",
        "name": "Opal history",
        "title": "Opal run history",
        "description": "All recorded Opal pipeline runs",
        "mimeType": "application/json"
    }));

    if history.last().is_some() {
        resources.push(json!({
            "uri": "opal://runs/latest",
            "name": "Latest Opal run",
            "title": "Latest Opal run",
            "description": "Most recent recorded Opal run",
            "mimeType": "application/json"
        }));
    }

    for entry in &history {
        resources.push(run_resource(entry));
        for job in &entry.jobs {
            resources.push(json!({
                "uri": format!(
                    "opal://runs/{}/jobs/{}/log",
                    encode_path_segment(&entry.run_id),
                    encode_path_segment(&job.name)
                ),
                "name": format!("{} log", job.name),
                "title": format!("{} • {} log", entry.run_id, job.name),
                "description": "Opal job log",
                "mimeType": "text/plain"
            }));
            if job.runtime_summary_path.is_some() {
                resources.push(json!({
                    "uri": format!(
                        "opal://runs/{}/jobs/{}/runtime-summary",
                        encode_path_segment(&entry.run_id),
                        encode_path_segment(&job.name)
                    ),
                    "name": format!("{} runtime summary", job.name),
                    "title": format!("{} • {} runtime summary", entry.run_id, job.name),
                    "description": "Opal job runtime summary",
                    "mimeType": "text/plain"
                }));
            }
        }
    }

    Ok(json!({ "resources": resources }))
}

pub(crate) fn read_resource(uri: &str) -> Result<Value> {
    match parse_resource_uri(uri)? {
        ResourceUri::History => {
            let history = load_history()?;
            text_resource(
                uri,
                "application/json",
                serde_json::to_string_pretty(&history)?,
            )
        }
        ResourceUri::LatestRun => {
            let Some(entry) = latest_history_entry()? else {
                anyhow::bail!("no Opal history entries found");
            };
            run_resource_contents(uri, &entry)
        }
        ResourceUri::Run { run_id } => {
            let entry = find_history_entry(&run_id)?
                .with_context(|| format!("run '{run_id}' not found in Opal history"))?;
            run_resource_contents(uri, &entry)
        }
        ResourceUri::JobLog { run_id, job_name } => {
            let entry = find_history_entry(&run_id)?
                .with_context(|| format!("run '{run_id}' not found in Opal history"))?;
            let job = find_job(&entry, &job_name)
                .with_context(|| format!("job '{job_name}' not found in run '{run_id}'"))?;
            text_resource(uri, "text/plain", read_job_log(&entry, job)?)
        }
        ResourceUri::RuntimeSummary { run_id, job_name } => {
            let entry = find_history_entry(&run_id)?
                .with_context(|| format!("run '{run_id}' not found in Opal history"))?;
            let job = find_job(&entry, &job_name)
                .with_context(|| format!("job '{job_name}' not found in run '{run_id}'"))?;
            let summary = read_runtime_summary(job)?
                .with_context(|| format!("job '{job_name}' has no runtime summary"))?;
            text_resource(uri, "text/plain", summary)
        }
    }
}

fn run_resource(entry: &HistoryEntry) -> Value {
    json!({
        "uri": format!("opal://runs/{}", encode_path_segment(&entry.run_id)),
        "name": entry.run_id,
        "title": format!("Opal run {}", entry.run_id),
        "description": "Recorded Opal pipeline run",
        "mimeType": "application/json"
    })
}

fn run_resource_contents(uri: &str, entry: &HistoryEntry) -> Result<Value> {
    text_resource(
        uri,
        "application/json",
        serde_json::to_string_pretty(entry)?,
    )
}

fn text_resource(uri: &str, mime_type: &str, text: String) -> Result<Value> {
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": mime_type,
            "text": text
        }]
    }))
}

#[cfg(test)]
mod tests {
    use super::{list_resources, read_resource};
    use crate::history::{HistoryEntry, HistoryJob, HistoryStatus, save};
    use crate::mcp::TEST_ENV_LOCK;
    use crate::runtime;
    use std::env;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn lists_history_and_run_resources() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-test-list");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-1".to_string(),
                finished_at: "now".to_string(),
                status: HistoryStatus::Success,
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

        let resources = list_resources().expect("list resources");
        let entries = resources["resources"].as_array().expect("resource array");
        assert!(entries.iter().any(|entry| entry["uri"] == "opal://history"));
        assert!(
            entries
                .iter()
                .any(|entry| entry["uri"] == "opal://runs/run-1")
        );
        unsafe {
            env::remove_var("OPAL_HOME");
        }
    }

    #[test]
    fn reads_history_resource() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-test-read");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("OPAL_HOME", &opal_home);
        }
        save(&runtime::history_path(), &[]).expect("save history");

        let resource = read_resource("opal://history").expect("read history");
        assert_eq!(resource["contents"][0]["mimeType"], "application/json");
        unsafe {
            env::remove_var("OPAL_HOME");
        }
    }
}
