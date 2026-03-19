use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default)]
pub(super) struct RuntimeState {
    job_attempts: Arc<Mutex<HashMap<String, usize>>>,
    running_containers: Arc<Mutex<HashMap<String, String>>>,
    cancelled_jobs: Arc<Mutex<HashSet<String>>>,
}

impl RuntimeState {
    pub(super) fn next_attempt(&self, job_name: &str) -> usize {
        let mut attempts = self
            .job_attempts
            .lock()
            .expect("job attempt tracker mutex poisoned");
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

    pub(super) fn running_containers(&self) -> Vec<(String, String)> {
        match self.running_containers.lock() {
            Ok(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Err(_) => Vec::new(),
        }
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
}

#[cfg(test)]
mod tests {
    use super::RuntimeState;

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

        state.clear_running_container("build");
        assert_eq!(state.running_container("build"), None);
    }
}
