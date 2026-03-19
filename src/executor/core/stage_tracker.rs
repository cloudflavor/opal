use crate::pipeline::StageState;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone)]
pub(super) struct StageTracker {
    positions: HashMap<String, usize>,
    states: Arc<Mutex<HashMap<String, StageState>>>,
}

impl StageTracker {
    pub(super) fn new(stages: &[(String, usize)]) -> Self {
        let mut positions = HashMap::new();
        let mut states = HashMap::new();
        for (idx, (name, total)) in stages.iter().enumerate() {
            positions.insert(name.clone(), idx);
            states.insert(name.clone(), StageState::new(*total));
        }
        Self {
            positions,
            states: Arc::new(Mutex::new(states)),
        }
    }

    pub(super) fn start(&self, stage_name: &str) -> bool {
        let mut states = self.states.lock().expect("stage tracker mutex poisoned");
        let state = states
            .entry(stage_name.to_string())
            .or_insert_with(|| StageState::new(0));
        if state.header_printed {
            false
        } else {
            state.header_printed = true;
            state.started_at = Some(Instant::now());
            true
        }
    }

    pub(super) fn complete_job(&self, stage_name: &str) -> Option<f32> {
        let mut states = self.states.lock().expect("stage tracker mutex poisoned");
        let state = states.get_mut(stage_name)?;
        state.completed += 1;
        if state.completed == state.total {
            state.started_at.map(|start| start.elapsed().as_secs_f32())
        } else {
            None
        }
    }

    pub(super) fn position(&self, stage_name: &str) -> usize {
        self.positions.get(stage_name).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::StageTracker;

    #[test]
    fn stage_tracker_starts_once_and_reports_final_completion() {
        let tracker = StageTracker::new(&[("build".into(), 2)]);

        assert!(tracker.start("build"));
        assert!(!tracker.start("build"));
        assert_eq!(tracker.position("build"), 0);
        assert_eq!(tracker.complete_job("build"), None);
        assert!(tracker.complete_job("build").is_some());
    }
}
