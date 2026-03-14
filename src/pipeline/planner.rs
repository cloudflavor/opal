use crate::gitlab::{Job, PipelineGraph};
use anyhow::{Result, anyhow};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

pub struct JobPlan {
    pub ordered: Vec<String>,
    pub nodes: HashMap<String, PlannedJob>,
    pub dependents: HashMap<String, Vec<String>>,
    pub order_index: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
pub struct PlannedJob {
    pub job: Job,
    pub stage_name: String,
    pub dependencies: Vec<String>,
    pub log_path: PathBuf,
    pub log_hash: String,
}

#[derive(Debug, Clone)]
pub struct JobSummary {
    pub name: String,
    pub stage_name: String,
    pub duration: f32,
    pub status: JobStatus,
    pub log_path: Option<PathBuf>,
    pub log_hash: String,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltKind {
    None,
    JobFailure,
    Deadlock,
    ChannelClosed,
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

pub fn build_job_plan<F>(graph: &PipelineGraph, mut log_info: F) -> Result<JobPlan>
where
    F: FnMut(&Job) -> (PathBuf, String),
{
    let mut nodes = HashMap::new();
    let mut ordered = Vec::new();

    for (stage_idx, stage) in graph.stages.iter().enumerate() {
        let default_deps: Vec<String> = if stage_idx == 0 {
            Vec::new()
        } else {
            graph.stages[stage_idx - 1]
                .jobs
                .iter()
                .map(|idx| graph.graph[*idx].name.clone())
                .collect()
        };

        for node_idx in &stage.jobs {
            let job = graph
                .graph
                .node_weight(*node_idx)
                .cloned()
                .ok_or_else(|| anyhow!("missing job for node"))?;

            let mut deps = if !job.needs.is_empty() {
                job.needs.iter().map(|need| need.job.clone()).collect()
            } else {
                default_deps.clone()
            };
            deps.sort();
            deps.dedup();

            let (log_path, log_hash) = log_info(&job);
            ordered.push(job.name.clone());
            nodes.insert(
                job.name.clone(),
                PlannedJob {
                    job,
                    stage_name: stage.name.clone(),
                    dependencies: deps,
                    log_path,
                    log_hash,
                },
            );
        }
    }

    let known_jobs: HashSet<String> = nodes.keys().cloned().collect();
    for planned in nodes.values_mut() {
        let mut missing_required = Vec::new();
        planned.dependencies.retain(|dep| {
            if known_jobs.contains(dep) {
                return true;
            }
            let is_optional = planned
                .job
                .needs
                .iter()
                .any(|need| need.job == *dep && need.optional);
            if !is_optional {
                missing_required.push(dep.clone());
            }
            false
        });

        if let Some(missing) = missing_required.first() {
            return Err(anyhow!(
                "job '{}' depends on unknown job '{}'",
                planned.job.name,
                missing
            ));
        }
    }

    let mut order_index = HashMap::new();
    for (idx, name) in ordered.iter().enumerate() {
        order_index.insert(name.clone(), idx);
    }

    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
    for (name, planned) in &nodes {
        for dep in &planned.dependencies {
            if !nodes.contains_key(dep) {
                return Err(anyhow!("job '{}' depends on unknown job '{}'", name, dep));
            }
            dependents
                .entry(dep.clone())
                .or_default()
                .push(name.clone());
        }
    }

    for deps in dependents.values_mut() {
        deps.sort_by_key(|name| order_index.get(name).copied().unwrap_or(usize::MAX));
    }

    Ok(JobPlan {
        ordered,
        nodes,
        dependents,
        order_index,
    })
}
