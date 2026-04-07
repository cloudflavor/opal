use crate::execution_plan::ExecutableJob;
use crate::pipeline::ResourceGroupManager;
use anyhow::{Error, anyhow};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio::{sync::mpsc, task, time as tokio_time};

const RESOURCE_GROUP_RETRY_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub(super) enum ResourceAcquire {
    Acquired,
    RetryScheduled,
    Failed(Error),
}

#[derive(Debug)]
pub(super) struct ResourceGroups {
    manager: ResourceGroupManager,
    retry_pending: HashSet<String>,
}

impl ResourceGroups {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            manager: ResourceGroupManager::new(root),
            retry_pending: HashSet::new(),
        }
    }

    pub(super) async fn acquire_for_job(
        &mut self,
        planned: &ExecutableJob,
        scheduler_idle: bool,
        delay_tx: &mpsc::UnboundedSender<String>,
    ) -> ResourceAcquire {
        let Some(group) = planned.instance.resource_group.as_deref() else {
            return ResourceAcquire::Acquired;
        };
        let owner = planned.instance.job.name.as_str();

        match self.try_acquire(group, owner).await {
            Ok(true) => ResourceAcquire::Acquired,
            Ok(false) if scheduler_idle => self.wait_until_acquired(group, owner).await,
            Ok(false) => {
                self.schedule_retry(owner, delay_tx);
                ResourceAcquire::RetryScheduled
            }
            Err(err) => ResourceAcquire::Failed(
                err.context(format!("failed to acquire resource group '{}'", group)),
            ),
        }
    }

    pub(super) fn consume_retry(&mut self, name: &str) -> bool {
        self.retry_pending.remove(name)
    }

    pub(super) fn retry_pending_is_empty(&self) -> bool {
        self.retry_pending.is_empty()
    }

    pub(super) async fn release(&self, planned: &ExecutableJob) {
        if let Some(group) = planned.instance.resource_group.as_deref() {
            let _ = self.release_group(group).await;
        }
    }

    async fn wait_until_acquired(&self, group: &str, owner: &str) -> ResourceAcquire {
        loop {
            tokio_time::sleep(RESOURCE_GROUP_RETRY_DELAY).await;
            match self.try_acquire(group, owner).await {
                Ok(true) => return ResourceAcquire::Acquired,
                Ok(false) => continue,
                Err(err) => {
                    return ResourceAcquire::Failed(
                        err.context(format!("failed to acquire resource group '{}'", group)),
                    );
                }
            }
        }
    }

    fn schedule_retry(&mut self, owner: &str, delay_tx: &mpsc::UnboundedSender<String>) {
        if self.retry_pending.insert(owner.to_string()) {
            let retry_name = owner.to_string();
            let tx_clone = delay_tx.clone();
            task::spawn(async move {
                tokio_time::sleep(RESOURCE_GROUP_RETRY_DELAY).await;
                let _ = tx_clone.send(retry_name);
            });
        }
    }

    async fn try_acquire(&self, group: &str, owner: &str) -> Result<bool, Error> {
        let manager = self.manager.clone();
        let group = group.to_string();
        let owner = owner.to_string();
        task::spawn_blocking(move || manager.try_acquire(&group, &owner))
            .await
            .map_err(|err| anyhow!("resource group acquire task failed: {err}"))?
    }

    async fn release_group(&self, group: &str) -> Result<(), Error> {
        let manager = self.manager.clone();
        let group = group.to_string();
        task::spawn_blocking(move || manager.release(&group))
            .await
            .map_err(|err| anyhow!("resource group release task failed: {err}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::{ResourceAcquire, ResourceGroups};
    use crate::compiler::JobInstance;
    use crate::execution_plan::ExecutableJob;
    use crate::model::{ArtifactSpec, JobSpec, RetryPolicySpec};
    use crate::pipeline::{ResourceGroupManager, RuleEvaluation, RuleWhen};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn acquire_for_job_schedules_retry_when_group_is_busy() {
        let dir = tempdir().expect("tempdir");
        let lock_root = dir.path().join("locks");
        let blocker = ResourceGroupManager::new(lock_root.clone());
        blocker
            .try_acquire("builder", "other-job")
            .expect("lock acquires");
        let mut groups = ResourceGroups::new(lock_root);
        let (delay_tx, mut delay_rx) = mpsc::unbounded_channel();

        let outcome = groups
            .acquire_for_job(&resource_job("build"), false, &delay_tx)
            .await;

        assert!(matches!(outcome, ResourceAcquire::RetryScheduled));
        let retry = tokio::time::timeout(Duration::from_secs(1), delay_rx.recv())
            .await
            .expect("delay event arrives");
        assert_eq!(retry.as_deref(), Some("build"));
        assert!(groups.consume_retry("build"));
    }

    #[tokio::test]
    async fn acquire_for_job_waits_until_group_is_available_when_scheduler_is_idle() {
        let dir = tempdir().expect("tempdir");
        let lock_root = dir.path().join("locks");
        let blocker = ResourceGroupManager::new(lock_root.clone());
        blocker
            .try_acquire("builder", "other-job")
            .expect("lock acquires");
        let mut groups = ResourceGroups::new(lock_root.clone());
        let (delay_tx, _delay_rx) = mpsc::unbounded_channel();

        let releaser = ResourceGroupManager::new(lock_root);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            releaser.release("builder").expect("lock releases");
        });

        let outcome = groups
            .acquire_for_job(&resource_job("build"), true, &delay_tx)
            .await;

        assert!(matches!(outcome, ResourceAcquire::Acquired));
        groups.release(&resource_job("build")).await;
        assert!(groups.retry_pending_is_empty());
    }

    fn resource_job(name: &str) -> ExecutableJob {
        ExecutableJob {
            instance: JobInstance {
                job: JobSpec {
                    name: name.into(),
                    stage: "build".into(),
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
                    when: None,
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
                    resource_group: Some("builder".into()),
                    parallel: None,
                    tags: Vec::new(),
                    environment: None,
                },
                stage_name: "build".into(),
                dependencies: Vec::new(),
                rule: RuleEvaluation {
                    included: true,
                    when: RuleWhen::OnSuccess,
                    ..Default::default()
                },
                timeout: None,
                retry: RetryPolicySpec::default(),
                interruptible: false,
                resource_group: Some("builder".into()),
            },
            log_path: PathBuf::from(format!("/tmp/{name}.log")),
            log_hash: format!("hash-{name}"),
        }
    }
}
