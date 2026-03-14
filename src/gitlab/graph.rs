use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use super::rules::JobRule;

#[derive(Debug, Clone)]
pub struct PipelineGraph {
    pub graph: DiGraph<Job, ()>,
    pub stages: Vec<StageGroup>,
    pub defaults: PipelineDefaults,
    pub workflow: Option<WorkflowConfig>,
    pub filters: PipelineFilters,
}
#[derive(Debug, Clone)]
pub struct StageGroup {
    pub name: String,
    pub jobs: Vec<NodeIndex>,
}
#[derive(Debug, Clone)]
pub struct Job {
    pub name: String,
    pub stage: String,
    pub commands: Vec<String>,
    pub needs: Vec<JobDependency>,
    pub explicit_needs: bool,
    pub dependencies: Vec<String>,
    pub before_script: Option<Vec<String>>,
    pub after_script: Option<Vec<String>>,
    pub rules: Vec<JobRule>,
    pub artifacts: Vec<PathBuf>,
    pub cache: Vec<CacheConfig>,
    pub image: Option<String>,
    pub variables: HashMap<String, String>,
    pub services: Vec<ServiceConfig>,
    pub timeout: Option<Duration>,
    pub retry: RetryPolicy,
    pub interruptible: bool,
    pub resource_group: Option<String>,
}
#[derive(Debug, Clone, Default)]
pub struct PipelineDefaults {
    pub image: Option<String>,
    pub before_script: Vec<String>,
    pub after_script: Vec<String>,
    pub variables: HashMap<String, String>,
    pub cache: Vec<CacheConfig>,
    pub services: Vec<ServiceConfig>,
    pub timeout: Option<Duration>,
    pub retry: RetryPolicy,
    pub interruptible: bool,
}
#[derive(Debug, Clone)]
pub struct JobDependency {
    pub job: String,
    pub needs_artifacts: bool,
    pub optional: bool,
    pub source: DependencySource,
}

#[derive(Debug, Clone)]
pub enum DependencySource {
    Local,
    External(ExternalDependency),
}

#[derive(Debug, Clone)]
pub struct ExternalDependency {
    pub project: String,
    pub reference: String,
}

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub image: String,
    pub alias: Option<String>,
    pub entrypoint: Vec<String>,
    pub command: Vec<String>,
    pub variables: HashMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct RetryPolicy {
    pub max: u32,
    pub when: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub key: String,
    pub paths: Vec<PathBuf>,
    pub policy: CachePolicy,
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowConfig {
    pub rules: Vec<JobRule>,
}

#[derive(Debug, Clone, Default)]
pub struct PipelineFilters {
    pub only: Vec<String>,
    pub except: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    Pull,
    Push,
    PullPush,
}
impl CachePolicy {
    pub(crate) fn from_str(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "pull" => CachePolicy::Pull,
            "push" => CachePolicy::Push,
            _ => CachePolicy::PullPush,
        }
    }

    pub fn allows_pull(self) -> bool {
        matches!(self, CachePolicy::Pull | CachePolicy::PullPush)
    }

    pub fn allows_push(self) -> bool {
        matches!(self, CachePolicy::Push | CachePolicy::PullPush)
    }
}
