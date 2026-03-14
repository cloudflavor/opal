use crate::pipeline::{Job, PipelineGraph};
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::path::PathBuf;

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
