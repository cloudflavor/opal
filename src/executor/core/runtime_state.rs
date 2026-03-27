use crate::model::ArtifactSourceOutcome;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default)]
pub(super) struct RuntimeState {
    job_attempts: Arc<Mutex<HashMap<String, usize>>>,
    running_containers: Arc<Mutex<HashMap<String, String>>>,
    runtime_objects: Arc<Mutex<HashMap<String, RuntimeObjects>>>,
    cancelled_jobs: Arc<Mutex<HashSet<String>>>,
    completed_jobs: Arc<Mutex<HashMap<String, ArtifactSourceOutcome>>>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct RuntimeObjects {
    pub container_name: Option<String>,
    pub service_network: Option<String>,
    pub service_containers: Vec<String>,
}

impl RuntimeState {
    pub(super) fn next_attempt(&self, job_name: &str) -> usize {
        let mut attempts = match self.job_attempts.lock() {
            Ok(attempts) => attempts,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry = attempts.entry(job_name.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }

    pub(super) fn track_running_container(&self, job_name: &str, container: &str) {
        if let Ok(mut map) = self.running_containers.lock() {
            map.insert(job_name.to_string(), container.to_string());
        }
    }

    pub(super) fn clear_running_container(&self, job_name: &str) {
        if let Ok(mut map) = self.running_containers.lock() {
            map.remove(job_name);
        }
    }

    pub(super) fn running_container(&self, job_name: &str) -> Option<String> {
        let map = self.running_containers.lock().ok()?;
        map.get(job_name).cloned()
    }

    pub(super) fn record_runtime_objects(
        &self,
        job_name: &str,
        container_name: String,
        service_network: Option<String>,
        service_containers: Vec<String>,
    ) {
        if let Ok(mut map) = self.runtime_objects.lock() {
            map.insert(
                job_name.to_string(),
                RuntimeObjects {
                    container_name: Some(container_name),
                    service_network,
                    service_containers,
                },
            );
        }
    }

    pub(super) fn runtime_objects(&self, job_name: &str) -> Option<RuntimeObjects> {
        let map = self.runtime_objects.lock().ok()?;
        map.get(job_name).cloned()
    }

    pub(super) fn mark_job_cancelled(&self, job_name: &str) {
        if let Ok(mut cancelled) = self.cancelled_jobs.lock() {
            cancelled.insert(job_name.to_string());
        }
    }

    pub(super) fn take_cancelled_job(&self, job_name: &str) -> bool {
        if let Ok(mut cancelled) = self.cancelled_jobs.lock() {
            cancelled.remove(job_name)
        } else {
            false
        }
    }

    pub(super) fn record_completed_job(&self, job_name: &str, outcome: ArtifactSourceOutcome) {
        if let Ok(mut completed) = self.completed_jobs.lock() {
            completed.insert(job_name.to_string(), outcome);
        }
    }

    pub(super) fn completed_jobs(&self) -> HashMap<String, ArtifactSourceOutcome> {
        match self.completed_jobs.lock() {
            Ok(map) => map.clone(),
            Err(_) => HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeState;
    use crate::model::ArtifactSourceOutcome;

    #[test]
    fn runtime_state_tracks_attempts_and_cancellation() {
        let state = RuntimeState::default();

        assert_eq!(state.next_attempt("build"), 1);
        assert_eq!(state.next_attempt("build"), 2);

        state.track_running_container("build", "opal-build-01");
        assert_eq!(
            state.running_container("build").as_deref(),
            Some("opal-build-01")
        );

        state.mark_job_cancelled("build");
        assert!(state.take_cancelled_job("build"));
        assert!(!state.take_cancelled_job("build"));

        state.record_completed_job("build", ArtifactSourceOutcome::Success);
        assert_eq!(
            state.completed_jobs().get("build"),
            Some(&ArtifactSourceOutcome::Success)
        );

        state.clear_running_container("build");
        assert_eq!(state.running_container("build"), None);
    }
}
