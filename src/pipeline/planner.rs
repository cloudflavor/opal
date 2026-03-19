use super::rules::RuleContext;
use crate::compiler::compile_pipeline;
use crate::execution_plan::{ExecutableJob, ExecutionPlan, build_execution_plan};
use crate::model::{EnvironmentSpec, PipelineSpec};
use anyhow::Result;
use std::path::PathBuf;
use std::time::Instant;

pub type JobPlan = ExecutionPlan;
pub type PlannedJob = ExecutableJob;

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
    log_info: F,
) -> Result<JobPlan>
where
    F: FnMut(&crate::model::JobSpec) -> (std::path::PathBuf, String),
{
    let compiled = compile_pipeline(pipeline, rule_ctx)?;
    build_execution_plan(compiled, log_info)
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
                .instance
                .job
                .dependencies
                .iter()
                .any(|dep| dep == "build-matrix: [linux, release]")
        );
        assert!(
            package
                .instance
                .dependencies
                .iter()
                .any(|dep| dep == "build-matrix: [linux, release]")
        );
        let matrix_need = package
            .instance
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
            .instance
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
