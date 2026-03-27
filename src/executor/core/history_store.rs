use crate::history::{self, HistoryEntry, HistoryJob, HistoryStatus};
use crate::pipeline::{JobStatus, JobSummary};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use tracing::warn;

#[derive(Debug, Clone, Default)]
pub(super) struct HistoryResources {
    pub artifact_dir: Option<String>,
    pub artifacts: Vec<String>,
    pub caches: Vec<crate::history::HistoryCache>,
    pub container_name: Option<String>,
    pub service_network: Option<String>,
    pub service_containers: Vec<String>,
    pub runtime_summary_path: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct HistoryStore {
    path: PathBuf,
    entries: Arc<Mutex<Vec<HistoryEntry>>>,
}

impl HistoryStore {
    pub(super) fn load(path: PathBuf) -> Self {
        let entries = match history::load(&path) {
            Ok(entries) => entries,
            Err(err) => {
                warn!(
                    error = %err,
                    path = %path.display(),
                    "failed to load pipeline history"
                );
                Vec::new()
            }
        };
        Self {
            path,
            entries: Arc::new(Mutex::new(entries)),
        }
    }

    pub(super) fn snapshot(&self) -> Vec<HistoryEntry> {
        self.entries
            .lock()
            .map(|entries| entries.clone())
            .unwrap_or_default()
    }

    pub(super) fn record(
        &self,
        run_id: &str,
        summaries: &[JobSummary],
        resources: &HashMap<String, HistoryResources>,
    ) -> Option<HistoryEntry> {
        let finished_at = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".to_string());
        let entry = HistoryEntry {
            run_id: run_id.to_string(),
            finished_at,
            status: pipeline_status(summaries),
            jobs: summaries
                .iter()
                .map(|summary| HistoryJob {
                    name: summary.name.clone(),
                    stage: summary.stage_name.clone(),
                    status: history_status_for_job(&summary.status),
                    log_hash: summary.log_hash.clone(),
                    log_path: summary
                        .log_path
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    artifact_dir: resources
                        .get(&summary.name)
                        .and_then(|info| info.artifact_dir.clone()),
                    artifacts: resources
                        .get(&summary.name)
                        .map(|info| info.artifacts.clone())
                        .unwrap_or_default(),
                    caches: resources
                        .get(&summary.name)
                        .map(|info| info.caches.clone())
                        .unwrap_or_default(),
                    container_name: resources
                        .get(&summary.name)
                        .and_then(|info| info.container_name.clone()),
                    service_network: resources
                        .get(&summary.name)
                        .and_then(|info| info.service_network.clone()),
                    service_containers: resources
                        .get(&summary.name)
                        .map(|info| info.service_containers.clone())
                        .unwrap_or_default(),
                    runtime_summary_path: resources
                        .get(&summary.name)
                        .and_then(|info| info.runtime_summary_path.clone()),
                })
                .collect(),
        };

        match self.entries.lock() {
            Ok(mut existing) => {
                existing.push(entry.clone());
                if let Some(parent) = self.path.parent()
                    && let Err(err) = fs::create_dir_all(parent)
                {
                    warn!(
                        error = %err,
                        path = %parent.display(),
                        "failed to create history directory"
                    );
                    return None;
                }
                if let Err(err) = history::save(&self.path, &existing) {
                    warn!(error = %err, "failed to persist pipeline history");
                }
                Some(entry)
            }
            Err(err) => {
                warn!(error = %err, "failed to record pipeline history");
                None
            }
        }
    }
}

fn pipeline_status(summaries: &[JobSummary]) -> HistoryStatus {
    if summaries
        .iter()
        .any(|entry| matches!(entry.status, JobStatus::Failed(_)) && !entry.allow_failure)
    {
        HistoryStatus::Failed
    } else if summaries
        .iter()
        .all(|entry| matches!(entry.status, JobStatus::Skipped(_)))
    {
        HistoryStatus::Skipped
    } else {
        HistoryStatus::Success
    }
}

fn history_status_for_job(status: &JobStatus) -> HistoryStatus {
    match status {
        JobStatus::Success => HistoryStatus::Success,
        JobStatus::Failed(_) => HistoryStatus::Failed,
        JobStatus::Skipped(_) => HistoryStatus::Skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::{HistoryResources, HistoryStore};
    use crate::history::{HistoryCache, HistoryStatus};
    use crate::pipeline::{JobStatus, JobSummary};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn history_store_records_pipeline_entry() {
        let path = temp_path("history-store").join("history.json");
        let store = HistoryStore::load(path.clone());
        let summaries = vec![JobSummary {
            name: "build".into(),
            stage_name: "test".into(),
            duration: 1.0,
            status: JobStatus::Success,
            log_path: Some(PathBuf::from("/tmp/build.log")),
            log_hash: "hash-build".into(),
            allow_failure: false,
            environment: None,
        }];
        let resources = HashMap::from([(
            "build".into(),
            HistoryResources {
                artifact_dir: Some("/tmp/artifacts".into()),
                artifacts: vec!["dist/".into()],
                caches: vec![HistoryCache {
                    key: "cache".into(),
                    policy: "pull-push".into(),
                    host: "/tmp/cache".into(),
                    paths: vec!["target".into()],
                }],
                container_name: Some("opal-build-01".into()),
                service_network: Some("opal-net-build".into()),
                service_containers: vec!["opal-svc-build-00".into()],
                runtime_summary_path: Some("/tmp/runtime/inspect.txt".into()),
            },
        )]);

        let entry = store
            .record("run-123", &summaries, &resources)
            .expect("history entry recorded");

        assert_eq!(entry.run_id, "run-123");
        assert_eq!(entry.status, HistoryStatus::Success);
        assert_eq!(entry.jobs.len(), 1);
        assert_eq!(
            entry.jobs[0].artifact_dir.as_deref(),
            Some("/tmp/artifacts")
        );
        assert_eq!(entry.jobs[0].artifacts, vec!["dist/"]);
        assert_eq!(entry.jobs[0].caches[0].key, "cache");
        assert_eq!(
            entry.jobs[0].container_name.as_deref(),
            Some("opal-build-01")
        );
        assert_eq!(
            entry.jobs[0].service_network.as_deref(),
            Some("opal-net-build")
        );
        assert_eq!(entry.jobs[0].service_containers, vec!["opal-svc-build-00"]);
        assert_eq!(store.snapshot().len(), 1);
        assert!(path.exists());
        let _ = std::fs::remove_file(path);
    }

    fn temp_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("opal-{prefix}-{nanos}"))
    }
}
