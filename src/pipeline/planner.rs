use super::rules::{RuleContext, RuleEvaluation};
use crate::compiler::{CompiledPipeline, JobInstance, JobVariantInfo, compile_pipeline};
use crate::model::{EnvironmentSpec, JobDependencySpec, JobSpec, PipelineSpec, RetryPolicySpec};
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct JobPlan {
    pub ordered: Vec<String>,
    pub nodes: HashMap<String, PlannedJob>,
    pub dependents: HashMap<String, Vec<String>>,
    pub order_index: HashMap<String, usize>,
    pub variants: HashMap<String, Vec<JobVariantInfo>>,
}

impl JobPlan {
    pub fn variants_for_dependency(&self, dep: &JobDependencySpec) -> Vec<String> {
        let compiled = CompiledPipeline {
            ordered: self.ordered.clone(),
            jobs: self
                .nodes
                .iter()
                .map(|(name, planned)| {
                    (
                        name.clone(),
                        JobInstance {
                            job: planned.job.clone(),
                            stage_name: planned.stage_name.clone(),
                            dependencies: planned.dependencies.clone(),
                            rule: planned.rule.clone(),
                            timeout: planned.timeout,
                            retry: planned.retry.clone(),
                            interruptible: planned.interruptible,
                            resource_group: planned.resource_group.clone(),
                        },
                    )
                })
                .collect(),
            dependents: self.dependents.clone(),
            order_index: self.order_index.clone(),
            variants: self.variants.clone(),
        };
        compiled.variants_for_dependency(dep)
    }
}

#[derive(Debug, Clone)]
pub struct PlannedJob {
    pub job: JobSpec,
    pub stage_name: String,
    pub dependencies: Vec<String>,
    pub log_path: PathBuf,
    pub log_hash: String,
    pub rule: RuleEvaluation,
    pub timeout: Option<Duration>,
    pub retry: RetryPolicySpec,
    pub interruptible: bool,
    pub resource_group: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JobSummary {
    pub name: String,
    pub stage_name: String,
    pub duration: f32,
    pub status: JobStatus,
    pub log_path: Option<PathBuf>,
    pub log_hash: String,
    pub allow_failure: bool,
    pub environment: Option<EnvironmentSpec>,
}

#[derive(Debug, Clone)]
pub enum JobStatus {
    Success,
    Failed(String),
    Skipped(String),
}

#[derive(Debug, Clone)]
pub struct JobRunInfo {
    pub container_name: String,
}

#[derive(Debug)]
pub struct JobEvent {
    pub name: String,
    pub stage_name: String,
    pub duration: f32,
    pub log_path: Option<PathBuf>,
    pub log_hash: String,
    pub result: Result<()>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltKind {
    None,
    JobFailure,
    Deadlock,
    ChannelClosed,
    Aborted,
}

#[derive(Debug, Clone)]
pub struct StageState {
    pub total: usize,
    pub completed: usize,
    pub header_printed: bool,
    pub started_at: Option<Instant>,
}

impl StageState {
    pub fn new(total: usize) -> Self {
        Self {
            total,
            completed: 0,
            header_printed: false,
            started_at: None,
        }
    }
}

pub fn build_job_plan<F>(
    pipeline: &PipelineSpec,
    rule_ctx: Option<&RuleContext>,
    mut log_info: F,
) -> Result<JobPlan>
where
    F: FnMut(&JobSpec) -> (PathBuf, String),
{
    let CompiledPipeline {
        ordered,
        jobs,
        dependents,
        order_index,
        variants,
    } = compile_pipeline(pipeline, rule_ctx)?;
    let mut nodes = HashMap::new();
    for name in &ordered {
        let compiled = jobs
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("compiled job '{}' missing from output", name))?;
        let (log_path, log_hash) = log_info(&compiled.job);
        nodes.insert(
            name.clone(),
            PlannedJob {
                job: compiled.job,
                stage_name: compiled.stage_name,
                dependencies: compiled.dependencies,
                log_path,
                log_hash,
                rule: compiled.rule,
                timeout: compiled.timeout,
                retry: compiled.retry,
                interruptible: compiled.interruptible,
                resource_group: compiled.resource_group,
            },
        );
    }
    Ok(JobPlan {
        ordered,
        nodes,
        dependents,
        order_index,
        variants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PipelineSpec;
    use crate::pipeline::rules::RuleContext;
    use std::path::{Path, PathBuf};

    #[test]
    fn resolves_matrix_needs_to_variant_names() {
        let pipeline = PipelineSpec::from_path(Path::new(
            "pipelines/tests/needs-and-artifacts.gitlab-ci.yml",
        ))
        .unwrap();
        let ctx = RuleContext::new(Path::new("."));
        let plan = build_job_plan(&pipeline, Some(&ctx), |_job| {
            (PathBuf::new(), String::new())
        })
        .unwrap();
        assert!(plan.nodes.contains_key("build-matrix: [linux, release]"));
        let package = plan.nodes.get("package-linux").expect("package job exists");
        assert!(
            package
                .job
                .dependencies
                .iter()
                .any(|dep| dep == "build-matrix: [linux, release]")
        );
        assert!(
            package
                .dependencies
                .iter()
                .any(|dep| dep == "build-matrix: [linux, release]")
        );
        let matrix_need = package
            .job
            .needs
            .iter()
            .find(|need| need.job == "build-matrix")
            .expect("matrix dependency present");
        let variants = plan.variants_for_dependency(matrix_need);
        assert_eq!(variants, vec!["build-matrix: [linux, release]".to_string()]);
    }

    #[test]
    fn package_needs_tracks_inline_variant() {
        let pipeline = PipelineSpec::from_path(Path::new(
            "pipelines/tests/needs-and-artifacts.gitlab-ci.yml",
        ))
        .unwrap();
        let ctx = RuleContext::new(Path::new("."));
        let plan = build_job_plan(&pipeline, Some(&ctx), |_job| {
            (PathBuf::new(), String::new())
        })
        .unwrap();
        let package = plan.nodes.get("package-linux").expect("package job exists");
        let matrix_need = package
            .job
            .needs
            .iter()
            .find(|need| need.job == "build-matrix")
            .expect("matrix dependency present");
        assert_eq!(
            matrix_need.inline_variant,
            Some(vec!["linux".to_string(), "release".to_string()])
        );
    }
}
