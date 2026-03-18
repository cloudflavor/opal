use crate::gitlab::{
    DependencySource, EnvironmentConfig, Job, JobDependency, ParallelConfig, ParallelMatrixEntry,
    PipelineGraph, RetryPolicy,
};
use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::warn;

use super::rules::{RuleContext, RuleEvaluation, evaluate_rules};

#[derive(Clone, Debug)]
pub struct JobVariantInfo {
    pub name: String,
    pub labels: HashMap<String, String>,
    pub ordered_values: Vec<String>,
}

#[derive(Clone)]
struct LabelCombination {
    ordered: Vec<(String, String)>,
    lookup: HashMap<String, String>,
}

impl LabelCombination {
    fn empty() -> Self {
        Self {
            ordered: Vec::new(),
            lookup: HashMap::new(),
        }
    }

    fn push(&self, key: String, value: String) -> Self {
        let mut ordered = self.ordered.clone();
        ordered.push((key.clone(), value.clone()));
        let mut lookup = self.lookup.clone();
        lookup.insert(key, value);
        Self { ordered, lookup }
    }
}

struct ExpandedVariant {
    job: Job,
    labels: HashMap<String, String>,
    base_name: String,
    ordered_values: Vec<String>,
}

pub struct JobPlan {
    pub ordered: Vec<String>,
    pub nodes: HashMap<String, PlannedJob>,
    pub dependents: HashMap<String, Vec<String>>,
    pub order_index: HashMap<String, usize>,
    pub variants: HashMap<String, Vec<JobVariantInfo>>,
}

impl JobPlan {
    pub fn variants_for_dependency(&self, dep: &JobDependency) -> Vec<String> {
        let Some(entries) = self.variants.get(&dep.job) else {
            return Vec::new();
        };
        select_variants(entries, dep)
            .into_iter()
            .map(|variant| variant.name.clone())
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct PlannedJob {
    pub job: Job,
    pub stage_name: String,
    pub dependencies: Vec<String>,
    pub log_path: PathBuf,
    pub log_hash: String,
    pub rule: RuleEvaluation,
    pub timeout: Option<Duration>,
    pub retry: RetryPolicy,
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
    pub environment: Option<EnvironmentConfig>,
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
    graph: &PipelineGraph,
    rule_ctx: Option<&RuleContext>,
    mut log_info: F,
) -> Result<JobPlan>
where
    F: FnMut(&Job) -> (PathBuf, String),
{
    let mut nodes = HashMap::new();
    let mut ordered = Vec::new();
    let mut expanded_jobs: HashMap<String, Vec<ExpandedVariant>> = HashMap::new();
    let mut variant_lookup: HashMap<String, Vec<JobVariantInfo>> = HashMap::new();

    for node_idx in graph.graph.node_indices() {
        let base_job = graph
            .graph
            .node_weight(node_idx)
            .cloned()
            .ok_or_else(|| anyhow!("missing job for node"))?;
        let variants = expand_job_variants(base_job.clone())?;
        variant_lookup.insert(
            base_job.name.clone(),
            variants
                .iter()
                .map(|variant| JobVariantInfo {
                    name: variant.job.name.clone(),
                    labels: variant.labels.clone(),
                    ordered_values: variant.ordered_values.clone(),
                })
                .collect(),
        );
        expanded_jobs.insert(base_job.name.clone(), variants);
    }

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
            let base_job = graph
                .graph
                .node_weight(*node_idx)
                .cloned()
                .ok_or_else(|| anyhow!("missing job for node"))?;
            let base_name = base_job.name.clone();
            let variants = match expanded_jobs.remove(&base_name) {
                Some(list) => list,
                None => expand_job_variants(base_job.clone())?,
            };
            for mut expanded in variants {
                let evaluation = if let Some(ctx) = rule_ctx {
                    evaluate_rules(&expanded.job, ctx)?
                } else {
                    RuleEvaluation::default()
                };
                if !evaluation.included {
                    if let Some(entry) = variant_lookup.get_mut(&expanded.base_name) {
                        entry.retain(|meta| meta.name != expanded.job.name);
                    }
                    continue;
                }
                if !expanded.job.tags.is_empty() {
                    warn!(
                        job = %expanded.job.name,
                        tags = ?expanded.job.tags,
                        "job has runner tags, but Opal runs locally; ignoring tags"
                    );
                }
                if !evaluation.variables.is_empty() {
                    expanded.job.variables.extend(evaluation.variables.clone());
                }
                let resolved_deps = if expanded.job.explicit_needs {
                    resolve_parallel_dependencies(
                        &expanded.job.name,
                        &expanded.job.needs,
                        &variant_lookup,
                    )?
                } else {
                    resolve_default_dependencies(&default_deps, &variant_lookup)
                };
                let (log_path, log_hash) = log_info(&expanded.job);
                let job_timeout = expanded.job.timeout;
                let job_retry = expanded.job.retry.clone();
                let job_interruptible = expanded.job.interruptible;
                let job_resource_group = expanded.job.resource_group.clone();
                let job_name = expanded.job.name.clone();
                let job_stage = stage.name.clone();
                ordered.push(job_name.clone());
                nodes.insert(
                    job_name.clone(),
                    PlannedJob {
                        job: expanded.job,
                        stage_name: job_stage,
                        dependencies: resolved_deps,
                        log_path,
                        log_hash,
                        rule: evaluation.clone(),
                        timeout: job_timeout,
                        retry: job_retry,
                        interruptible: job_interruptible,
                        resource_group: job_resource_group,
                    },
                );
            }
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
        variants: variant_lookup,
    })
}

fn resolve_parallel_dependencies(
    owner: &str,
    deps: &[JobDependency],
    variant_lookup: &HashMap<String, Vec<JobVariantInfo>>,
) -> Result<Vec<String>> {
    let mut resolved = Vec::new();
    for dep in deps {
        if !matches!(dep.source, DependencySource::Local) {
            continue;
        }
        let Some(variants) = variant_lookup.get(&dep.job) else {
            if dep.optional {
                continue;
            } else {
                return Err(anyhow!(
                    "job '{}' depends on unknown job '{}'",
                    owner,
                    dep.job
                ));
            }
        };
        let selected = select_variants(variants, dep);
        if selected.is_empty() {
            if dep.optional {
                continue;
            } else {
                return Err(anyhow!(
                    "job '{}' depends on '{}', but no parallel variant matches the requested matrix",
                    owner,
                    dep.job
                ));
            }
        }
        resolved.extend(selected.into_iter().map(|variant| variant.name.clone()));
    }
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

fn resolve_default_dependencies(
    defaults: &[String],
    variant_lookup: &HashMap<String, Vec<JobVariantInfo>>,
) -> Vec<String> {
    let mut deps = Vec::new();
    for name in defaults {
        if let Some(variants) = variant_lookup.get(name) {
            deps.extend(variants.iter().map(|variant| variant.name.clone()));
        }
    }
    deps.sort();
    deps.dedup();
    deps
}

fn select_variants<'a>(
    variants: &'a [JobVariantInfo],
    dep: &JobDependency,
) -> Vec<&'a JobVariantInfo> {
    if let Some(filters) = &dep.parallel {
        variants
            .iter()
            .filter(|variant| {
                filters.iter().any(|filter| {
                    filter.iter().all(|(key, value)| {
                        variant
                            .labels
                            .get(key)
                            .map(|current| current == value)
                            .unwrap_or(false)
                    })
                })
            })
            .collect()
    } else if let Some(expected) = &dep.inline_variant {
        variants
            .iter()
            .filter(|variant| &variant.ordered_values == expected)
            .collect()
    } else {
        variants.iter().collect()
    }
}

fn expand_job_variants(job: Job) -> Result<Vec<ExpandedVariant>> {
    let base_name = job.name.clone();
    let mut variants = Vec::new();
    match &job.parallel {
        Some(ParallelConfig::Count(count)) => {
            let total = (*count).max(1);
            for idx in 0..total {
                let mut clone = job.clone();
                clone.parallel = None;
                clone.name = format!("{}: [{}]", base_name, idx + 1);
                clone
                    .variables
                    .insert("CI_NODE_INDEX".into(), (idx + 1).to_string());
                clone
                    .variables
                    .insert("CI_NODE_TOTAL".into(), total.to_string());
                variants.push(ExpandedVariant {
                    job: clone,
                    labels: HashMap::new(),
                    base_name: base_name.clone(),
                    ordered_values: vec![(idx + 1).to_string()],
                });
            }
        }
        Some(ParallelConfig::Matrix(entries)) => {
            let combos = matrix_combinations(entries)?;
            if combos.len() > 200 {
                bail!(
                    "parallel matrix for '{}' produces {} combinations, exceeding the limit of 200",
                    base_name,
                    combos.len()
                );
            }
            let total = combos.len();
            for (idx, combo) in combos.into_iter().enumerate() {
                let mut clone = job.clone();
                clone.parallel = None;
                let label_text = format_gitlab_variant_values(&combo.ordered);
                clone.name = format!("{}: [{}]", base_name, label_text);
                for (key, value) in &combo.ordered {
                    clone.variables.insert(key.clone(), value.clone());
                }
                clone
                    .variables
                    .insert("CI_NODE_INDEX".into(), (idx + 1).to_string());
                clone
                    .variables
                    .insert("CI_NODE_TOTAL".into(), total.to_string());
                let ordered_values = combo
                    .ordered
                    .iter()
                    .map(|(_, value)| value.clone())
                    .collect();
                variants.push(ExpandedVariant {
                    job: clone,
                    labels: combo.lookup.clone(),
                    base_name: base_name.clone(),
                    ordered_values,
                });
            }
        }
        None => {
            let mut clone = job.clone();
            clone.parallel = None;
            variants.push(ExpandedVariant {
                job: clone,
                labels: HashMap::new(),
                base_name,
                ordered_values: Vec::new(),
            });
            return Ok(variants);
        }
    }
    Ok(variants)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::rules::RuleContext;
    use std::path::{Path, PathBuf};

    #[test]
    fn resolves_matrix_needs_to_variant_names() {
        let graph =
            PipelineGraph::from_path("pipelines/tests/needs-and-artifacts.gitlab-ci.yml").unwrap();
        let ctx = RuleContext::new(Path::new("."));
        let plan =
            build_job_plan(&graph, Some(&ctx), |_job| (PathBuf::new(), String::new())).unwrap();
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
        let graph =
            PipelineGraph::from_path("pipelines/tests/needs-and-artifacts.gitlab-ci.yml").unwrap();
        let ctx = RuleContext::new(Path::new("."));
        let plan =
            build_job_plan(&graph, Some(&ctx), |_job| (PathBuf::new(), String::new())).unwrap();
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

fn matrix_combinations(entries: &[ParallelMatrixEntry]) -> Result<Vec<LabelCombination>> {
    if entries.is_empty() {
        return Ok(vec![LabelCombination::empty()]);
    }
    let mut combos = Vec::new();
    for entry in entries {
        let mut entry_combos = vec![LabelCombination::empty()];
        for var in &entry.variables {
            let mut new_sets = Vec::new();
            for combo in &entry_combos {
                for value in &var.values {
                    new_sets.push(combo.push(var.name.clone(), value.clone()));
                }
            }
            entry_combos = new_sets;
        }
        combos.extend(entry_combos);
    }
    Ok(combos)
}

fn format_gitlab_variant_values(labels: &[(String, String)]) -> String {
    labels
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>()
        .join(", ")
}
