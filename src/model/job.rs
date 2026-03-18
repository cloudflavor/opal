use super::{ArtifactSpec, CacheSpec, EnvironmentSpec, JobDependencySpec, ServiceSpec};
use crate::gitlab::rules::JobRule;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct JobSpec {
    pub name: String,
    pub stage: String,
    pub commands: Vec<String>,
    pub needs: Vec<JobDependencySpec>,
    pub explicit_needs: bool,
    pub dependencies: Vec<String>,
    pub before_script: Option<Vec<String>>,
    pub after_script: Option<Vec<String>>,
    pub inherit_default_before_script: bool,
    pub inherit_default_after_script: bool,
    pub rules: Vec<JobRule>,
    pub artifacts: ArtifactSpec,
    pub cache: Vec<CacheSpec>,
    pub image: Option<String>,
    pub variables: HashMap<String, String>,
    pub services: Vec<ServiceSpec>,
    pub timeout: Option<Duration>,
    pub retry: RetryPolicySpec,
    pub interruptible: bool,
    pub resource_group: Option<String>,
    pub parallel: Option<ParallelConfigSpec>,
    pub tags: Vec<String>,
    pub environment: Option<EnvironmentSpec>,
}

#[derive(Debug, Clone, Default)]
pub struct RetryPolicySpec {
    pub max: u32,
    pub when: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParallelConfigSpec {
    Count(u32),
    Matrix(Vec<ParallelMatrixEntrySpec>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParallelMatrixEntrySpec {
    pub variables: Vec<ParallelVariableSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParallelVariableSpec {
    pub name: String,
    pub values: Vec<String>,
}
