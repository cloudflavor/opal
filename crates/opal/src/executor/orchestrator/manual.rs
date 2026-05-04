use crate::execution_plan::ExecutableJob;
use crate::pipeline::RuleWhen;
use crate::ui::UiBridge;
use std::collections::HashSet;

#[derive(Debug)]
pub(super) enum ManualJobAction {
    NotManual,
    WaitForInput,
    Skip { reason: String },
}

#[derive(Debug, Default)]
pub(super) struct ManualJobs {
    waiting: HashSet<String>,
    approved: HashSet<String>,
    input_available: bool,
}

impl ManualJobs {
    pub(super) fn new(input_available: bool) -> Self {
        Self {
            waiting: HashSet::new(),
            approved: HashSet::new(),
            input_available,
        }
    }

    pub(super) fn classify(
        &mut self,
        planned: &ExecutableJob,
        ui: Option<&UiBridge>,
    ) -> ManualJobAction {
        if !matches!(planned.instance.rule.when, RuleWhen::Manual)
            || planned.instance.rule.manual_auto_run
        {
            return ManualJobAction::NotManual;
        }

        let name = planned.instance.job.name.clone();
        if self.approved.contains(&name) {
            return ManualJobAction::NotManual;
        }

        if self.input_available {
            if self.waiting.insert(name.clone())
                && let Some(ui_ref) = ui
            {
                ui_ref.job_manual_pending(&name);
            }
            ManualJobAction::WaitForInput
        } else {
            ManualJobAction::Skip {
                reason: manual_skip_reason(planned),
            }
        }
    }

    pub(super) fn start(&mut self, name: &str) -> bool {
        if self.waiting.remove(name) {
            self.approved.insert(name.to_string());
            true
        } else {
            false
        }
    }

    pub(super) fn close_input(&mut self) -> Vec<String> {
        self.input_available = false;
        self.waiting.drain().collect()
    }

    pub(super) fn clear(&mut self) {
        self.waiting.clear();
        self.approved.clear();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }
}

pub(super) fn manual_skip_reason(planned: &ExecutableJob) -> String {
    planned
        .instance
        .rule
        .manual_reason
        .clone()
        .unwrap_or_else(|| "manual job not run".to_string())
}

#[cfg(test)]
mod tests {
    use super::{ManualJobAction, ManualJobs, manual_skip_reason};
    use crate::compiler::JobInstance;
    use crate::execution_plan::ExecutableJob;
    use crate::model::{ArtifactSpec, JobSpec, RetryPolicySpec};
    use crate::pipeline::{RuleEvaluation, RuleWhen};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn manual_jobs_wait_for_user_input_when_available() {
        let mut manual = ManualJobs::new(true);
        let planned = manual_job("deploy", None);

        assert!(matches!(
            manual.classify(&planned, None),
            ManualJobAction::WaitForInput
        ));
        assert!(manual.start("deploy"));
        assert!(manual.is_empty());
    }

    #[test]
    fn manual_jobs_start_approves_next_classification() {
        let mut manual = ManualJobs::new(true);
        let planned = manual_job("deploy", None);

        assert!(matches!(
            manual.classify(&planned, None),
            ManualJobAction::WaitForInput
        ));
        assert!(manual.start("deploy"));
        assert!(matches!(
            manual.classify(&planned, None),
            ManualJobAction::NotManual
        ));
    }

    #[test]
    fn manual_jobs_skip_when_input_is_unavailable() {
        let mut manual = ManualJobs::new(false);
        let planned = manual_job("deploy", Some("manual approval required"));

        match manual.classify(&planned, None) {
            ManualJobAction::Skip { reason } => assert_eq!(reason, "manual approval required"),
            other => panic!("expected skip, got {other:?}"),
        }
    }

    #[test]
    fn manual_jobs_close_input_drains_pending_jobs() {
        let mut manual = ManualJobs::new(true);
        let planned = manual_job("deploy", None);

        assert!(matches!(
            manual.classify(&planned, None),
            ManualJobAction::WaitForInput
        ));
        assert_eq!(manual.close_input(), vec!["deploy".to_string()]);
        assert_eq!(manual_skip_reason(&planned), "manual job not run");
    }

    fn manual_job(name: &str, manual_reason: Option<&str>) -> ExecutableJob {
        ExecutableJob {
            instance: JobInstance {
                job: JobSpec {
                    name: name.into(),
                    stage: "deploy".into(),
                    commands: vec!["true".into()],
                    needs: Vec::new(),
                    explicit_needs: false,
                    dependencies: Vec::new(),
                    before_script: None,
                    after_script: None,
                    inherit_default_before_script: true,
                    inherit_default_after_script: true,
                    inherit_default_image: true,
                    inherit_default_cache: true,
                    inherit_default_services: true,
                    inherit_default_timeout: true,
                    inherit_default_retry: true,
                    inherit_default_interruptible: true,
                    when: Some("manual".into()),
                    rules: Vec::new(),
                    only: Vec::new(),
                    except: Vec::new(),
                    artifacts: ArtifactSpec::default(),
                    cache: Vec::new(),
                    image: None,
                    variables: HashMap::new(),
                    services: Vec::new(),
                    timeout: None,
                    retry: RetryPolicySpec::default(),
                    interruptible: false,
                    resource_group: None,
                    parallel: None,
                    tags: Vec::new(),
                    environment: None,
                },
                stage_name: "deploy".into(),
                dependencies: Vec::new(),
                rule: RuleEvaluation {
                    when: RuleWhen::Manual,
                    manual_reason: manual_reason.map(str::to_string),
                    ..Default::default()
                },
                timeout: None,
                retry: RetryPolicySpec::default(),
                interruptible: false,
                resource_group: None,
            },
            log_path: PathBuf::from(format!("/tmp/{name}.log")),
            log_hash: format!("hash-{name}"),
        }
    }
}
