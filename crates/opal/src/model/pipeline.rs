use super::{CacheSpec, ImageSpec, RetryPolicySpec, ServiceSpec};
use crate::gitlab::rules::JobRule;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct PipelineSpec {
    pub stages: Vec<StageSpec>,
    pub jobs: HashMap<String, super::JobSpec>,
    pub defaults: PipelineDefaultsSpec,
    pub workflow: Option<WorkflowSpec>,
    pub filters: PipelineFilterSpec,
}

#[derive(Debug, Clone)]
pub struct StageSpec {
    pub name: String,
    pub jobs: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PipelineDefaultsSpec {
    pub image: Option<ImageSpec>,
    pub before_script: Vec<String>,
    pub after_script: Vec<String>,
    pub variables: HashMap<String, String>,
    pub cache: Vec<CacheSpec>,
    pub services: Vec<ServiceSpec>,
    pub timeout: Option<Duration>,
    pub retry: RetryPolicySpec,
    pub interruptible: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowSpec {
    pub rules: Vec<JobRule>,
}

#[derive(Debug, Clone, Default)]
pub struct PipelineFilterSpec {
    pub only: Vec<String>,
    pub except: Vec<String>,
}
