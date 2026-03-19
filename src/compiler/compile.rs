use crate::compiler::{CompileContext, CompiledPipeline, JobInstance, JobVariantInfo};
use crate::model::{
    DependencySourceSpec, JobDependencySpec, JobSpec, ParallelConfigSpec, ParallelMatrixEntrySpec,
    PipelineSpec,
};
use crate::pipeline::rules::{RuleEvaluation, evaluate_rules};
use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use tracing::warn;

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
    job: JobSpec,
    labels: HashMap<String, String>,
    base_name: String,
    ordered_values: Vec<String>,
}

pub fn compile_pipeline(
    pipeline: &PipelineSpec,
    rule_ctx: Option<&CompileContext>,
) -> Result<CompiledPipeline> {
    let mut jobs = HashMap::new();
    let mut ordered = Vec::new();
    let mut expanded_jobs: HashMap<String, Vec<ExpandedVariant>> = HashMap::new();
    let mut variant_lookup: HashMap<String, Vec<JobVariantInfo>> = HashMap::new();

    for base_job in pipeline.jobs.values() {
        let base_job = base_job.clone();
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

    for (stage_idx, stage) in pipeline.stages.iter().enumerate() {
        let default_deps: Vec<String> = if stage_idx == 0 {
            Vec::new()
        } else {
            pipeline.stages[stage_idx - 1].jobs.clone()
        };

        for job_name in &stage.jobs {
            let base_job = pipeline
                .jobs
                .get(job_name)
                .cloned()
                .ok_or_else(|| anyhow!("missing job '{}'", job_name))?;
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
                let job_timeout = expanded.job.timeout;
                let job_retry = expanded.job.retry.clone();
                let job_interruptible = expanded.job.interruptible;
                let job_resource_group = expanded.job.resource_group.clone();
                let job_name = expanded.job.name.clone();
                let job_stage = stage.name.clone();
                ordered.push(job_name.clone());
                jobs.insert(
                    job_name.clone(),
                    JobInstance {
                        job: expanded.job,
                        stage_name: job_stage,
                        dependencies: resolved_deps,
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
    for (name, instance) in &jobs {
        for dep in &instance.dependencies {
            if !jobs.contains_key(dep) {
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

    Ok(CompiledPipeline {
        ordered,
        jobs,
        dependents,
        order_index,
        variants: variant_lookup,
    })
}

fn resolve_parallel_dependencies(
    owner: &str,
    deps: &[JobDependencySpec],
    variant_lookup: &HashMap<String, Vec<JobVariantInfo>>,
) -> Result<Vec<String>> {
    let mut resolved = Vec::new();
    for dep in deps {
        if !matches!(dep.source, DependencySourceSpec::Local) {
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
    dep: &JobDependencySpec,
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

fn expand_job_variants(job: JobSpec) -> Result<Vec<ExpandedVariant>> {
    let base_name = job.name.clone();
    let mut variants = Vec::new();
    match &job.parallel {
        Some(ParallelConfigSpec::Count(count)) => {
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
        Some(ParallelConfigSpec::Matrix(entries)) => {
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

fn matrix_combinations(entries: &[ParallelMatrixEntrySpec]) -> Result<Vec<LabelCombination>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitlab::rules::JobRule;
    use crate::model::{
        ArtifactSpec, PipelineDefaultsSpec, PipelineFilterSpec, PipelineSpec, RetryPolicySpec,
        StageSpec,
    };
    use crate::pipeline::rules::RuleContext;
    use std::path::Path;

    #[test]
    fn compile_pipeline_applies_rule_variables_and_excludes_unmatched_jobs() {
        let included = JobSpec {
            rules: vec![JobRule {
                if_expr: Some("$CI_COMMIT_BRANCH == \"main\"".into()),
                variables: HashMap::from([("FROM_RULE".into(), "1".into())]),
                ..JobRule::default()
            }],
            ..job("lint", "test")
        };
        let excluded = JobSpec {
            rules: vec![JobRule {
                if_expr: Some("$CI_COMMIT_BRANCH == \"release\"".into()),
                ..JobRule::default()
            }],
            ..job("publish", "deploy")
        };
        let pipeline = pipeline_spec(
            vec![
                StageSpec {
                    name: "test".into(),
                    jobs: vec!["lint".into()],
                },
                StageSpec {
                    name: "deploy".into(),
                    jobs: vec!["publish".into()],
                },
            ],
            vec![included, excluded],
        );
        let ctx = RuleContext::new(Path::new("."));

        let compiled = compile_pipeline(&pipeline, Some(&ctx)).expect("pipeline compiles");

        assert!(compiled.jobs.contains_key("lint"));
        assert!(!compiled.jobs.contains_key("publish"));
        assert_eq!(
            compiled.jobs["lint"].job.variables.get("FROM_RULE"),
            Some(&"1".to_string())
        );
        assert!(
            compiled
                .variants
                .get("publish")
                .is_none_or(|variants| variants.is_empty())
        );
    }

    #[test]
    fn compile_pipeline_resolves_matrix_needs_to_variant_dependencies() {
        let pipeline = PipelineSpec::from_path(Path::new(
            "pipelines/tests/needs-and-artifacts.gitlab-ci.yml",
        ))
        .expect("pipeline loads");
        let ctx = RuleContext::new(Path::new("."));

        let compiled = compile_pipeline(&pipeline, Some(&ctx)).expect("pipeline compiles");

        let package = compiled
            .jobs
            .get("package-linux")
            .expect("package job exists");
        assert!(compiled.jobs.contains_key("build-matrix: [linux, release]"));
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
        let variants = compiled.variants_for_dependency(matrix_need);
        assert_eq!(variants, vec!["build-matrix: [linux, release]".to_string()]);
    }

    fn pipeline_spec(stages: Vec<StageSpec>, jobs: Vec<JobSpec>) -> PipelineSpec {
        PipelineSpec {
            stages,
            jobs: jobs
                .into_iter()
                .map(|job| (job.name.clone(), job))
                .collect::<HashMap<_, _>>(),
            defaults: PipelineDefaultsSpec::default(),
            workflow: None,
            filters: PipelineFilterSpec::default(),
        }
    }

    fn job(name: &str, stage: &str) -> JobSpec {
        JobSpec {
            name: name.into(),
            stage: stage.into(),
            commands: vec!["true".into()],
            needs: Vec::new(),
            explicit_needs: false,
            dependencies: Vec::new(),
            before_script: None,
            after_script: None,
            inherit_default_before_script: true,
            inherit_default_after_script: true,
            rules: Vec::new(),
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
        }
    }
}
