use crate::app::OpalApp;
use crate::app::view::{
    find_history_entry_for_workdir, find_job, latest_history_entry_for_workdir,
    load_history_for_workdir, read_job_log, read_runtime_summary,
};
use crate::history::HistoryEntry;
use crate::mcp::uri::{ResourceUri, encode_path_segment, parse_resource_uri};
use anyhow::{Context, Result};
use include_dir::{Dir, File, include_dir};
use serde_json::{Value, json};
use std::path::Path;

static EMBEDDED_DOCS: Dir<'static> = include_dir!("$OPAL_EMBEDDED_DOCS_DIR");

pub(crate) async fn list_resources(app: &OpalApp) -> Result<Value> {
    let history = load_history_for_workdir(&app.resolve_workdir(None)).await?;
    let mut resources = Vec::new();
    resources.push(docs_index_resource());
    resources.extend(doc_files().into_iter().filter_map(doc_resource));
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

pub(crate) async fn read_resource(app: &OpalApp, uri: &str) -> Result<Value> {
    let workdir = app.resolve_workdir(None);
    match parse_resource_uri(uri)? {
        ResourceUri::DocsIndex => text_resource(
            uri,
            "application/json",
            serde_json::to_string_pretty(&embedded_docs_index())?,
        ),
        ResourceUri::Doc { path } => text_resource(uri, "text/markdown", read_embedded_doc(&path)?),
        ResourceUri::History => {
            let history = load_history_for_workdir(&workdir).await?;
            text_resource(
                uri,
                "application/json",
                serde_json::to_string_pretty(&history)?,
            )
        }
        ResourceUri::LatestRun => {
            let Some(entry) = latest_history_entry_for_workdir(&workdir).await? else {
                anyhow::bail!("no Opal history entries found");
            };
            run_resource_contents(uri, &entry)
        }
        ResourceUri::Run { run_id } => {
            let entry = find_history_entry_for_workdir(&workdir, &run_id)
                .await?
                .with_context(|| format!("run '{run_id}' not found in Opal history"))?;
            run_resource_contents(uri, &entry)
        }
        ResourceUri::JobLog { run_id, job_name } => {
            let entry = find_history_entry_for_workdir(&workdir, &run_id)
                .await?
                .with_context(|| format!("run '{run_id}' not found in Opal history"))?;
            let job = find_job(&entry, &job_name)
                .with_context(|| format!("job '{job_name}' not found in run '{run_id}'"))?;
            text_resource(uri, "text/plain", read_job_log(&entry, job).await?)
        }
        ResourceUri::RuntimeSummary { run_id, job_name } => {
            let entry = find_history_entry_for_workdir(&workdir, &run_id)
                .await?
                .with_context(|| format!("run '{run_id}' not found in Opal history"))?;
            let job = find_job(&entry, &job_name)
                .with_context(|| format!("job '{job_name}' not found in run '{run_id}'"))?;
            let summary = read_runtime_summary(job)
                .await?
                .with_context(|| format!("job '{job_name}' has no runtime summary"))?;
            text_resource(uri, "text/plain", summary)
        }
    }
}

fn docs_index_resource() -> Value {
    json!({
        "uri": "opal://docs",
        "name": "Opal embedded docs",
        "title": "Opal embedded documentation",
        "description": "Index of markdown documentation embedded in the Opal binary",
        "mimeType": "application/json"
    })
}

fn doc_resource(file: &'static File<'static>) -> Option<Value> {
    let path = file.path();
    let contents = file.contents_utf8()?;
    let uri = doc_uri(path)?;
    Some(json!({
        "uri": uri,
        "name": path.display().to_string(),
        "title": extract_doc_title(contents).unwrap_or_else(|| doc_title_from_path(path)),
        "description": "Embedded Opal documentation",
        "mimeType": "text/markdown",
        "size": file.contents().len()
    }))
}

pub(crate) fn embedded_docs_index() -> Value {
    let docs = doc_files()
        .into_iter()
        .filter_map(doc_resource)
        .collect::<Vec<_>>();
    json!({ "docs": docs })
}

pub(crate) fn read_embedded_doc(path: &Path) -> Result<String> {
    let file = find_doc_file(path)
        .with_context(|| format!("embedded doc '{}' not found", path.display()))?;
    let contents = file
        .contents_utf8()
        .with_context(|| format!("embedded doc '{}' is not valid UTF-8", path.display()))?;
    Ok(contents.to_string())
}

fn doc_files() -> Vec<&'static File<'static>> {
    let mut files = Vec::new();
    collect_doc_files(&EMBEDDED_DOCS, &mut files);
    files.sort_by_key(|file| file.path().display().to_string());
    files
}

fn collect_doc_files(dir: &'static Dir<'static>, files: &mut Vec<&'static File<'static>>) {
    for file in dir.files() {
        if is_markdown_path(file.path()) {
            files.push(file);
        }
    }
    for dir in dir.dirs() {
        collect_doc_files(dir, files);
    }
}

fn find_doc_file(path: &Path) -> Option<&'static File<'static>> {
    doc_files().into_iter().find(|file| file.path() == path)
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("md"))
        == Some(true)
}

fn doc_uri(path: &Path) -> Option<String> {
    let encoded_path = path
        .iter()
        .map(|segment| segment.to_str().map(encode_path_segment))
        .collect::<Option<Vec<_>>>()?
        .join("/");
    Some(format!("opal://docs/{encoded_path}"))
}

fn extract_doc_title(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let title = trimmed.trim_start_matches('#').trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

fn doc_title_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Document")
        .replace('_', " ")
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
    use crate::app::OpalApp;
    use crate::app::context::history_scope_root;
    use crate::history::{HistoryEntry, HistoryJob, HistoryStatus, save};
    use crate::mcp::TEST_ENV_LOCK;
    use crate::runtime;
    use std::env;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn lists_history_and_run_resources() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-test-list");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        let app = OpalApp::from_current_dir().expect("app");
        save(
            &runtime::history_path(),
            &[HistoryEntry {
                run_id: "run-1".to_string(),
                finished_at: "now".to_string(),
                status: HistoryStatus::Success,
                scope_root: Some(history_scope_root(&app.resolve_workdir(None))),
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

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let resources = runtime
            .block_on(list_resources(&app))
            .expect("list resources");
        let entries = resources["resources"].as_array().expect("resource array");
        assert!(entries.iter().any(|entry| entry["uri"] == "opal://history"));
        assert!(
            entries
                .iter()
                .any(|entry| entry["uri"] == "opal://runs/run-1")
        );
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn reads_history_resource() {
        let _guard = TEST_ENV_LOCK.lock().expect("lock env");
        let dir = tempdir().expect("tempdir");
        let opal_home = dir.path().join("opal-home-test-read");
        fs::create_dir_all(&opal_home).expect("opal home");
        unsafe {
            env::set_var("XDG_DATA_HOME", &opal_home);
        }
        save(&runtime::history_path(), &[]).expect("save history");

        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let resource = runtime
            .block_on(read_resource(&app, "opal://history"))
            .expect("read history");
        assert_eq!(resource["contents"][0]["mimeType"], "application/json");
        unsafe {
            env::remove_var("XDG_DATA_HOME");
        }
    }

    #[test]
    fn lists_embedded_doc_resources() {
        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let resources = runtime
            .block_on(list_resources(&app))
            .expect("list resources");
        let entries = resources["resources"].as_array().expect("resource array");
        assert!(entries.iter().any(|entry| entry["uri"] == "opal://docs"));
        assert!(
            entries
                .iter()
                .any(|entry| entry["uri"] == "opal://docs/index.md")
        );
    }

    #[test]
    fn reads_embedded_doc_resource() {
        let app = OpalApp::from_current_dir().expect("app");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let resource = runtime
            .block_on(read_resource(&app, "opal://docs/index.md"))
            .expect("read doc");
        assert_eq!(resource["contents"][0]["mimeType"], "text/markdown");
        assert!(
            resource["contents"][0]["text"]
                .as_str()
                .expect("text")
                .contains("# Opal Documentation")
        );
    }

    #[test]
    fn doc_uri_encodes_path_segments() {
        assert_eq!(
            super::doc_uri(Path::new("nested/help file.md")).expect("uri"),
            "opal://docs/nested/help%20file.md"
        );
    }
}
