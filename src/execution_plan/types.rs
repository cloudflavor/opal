use crate::compiler::JobVariantInfo;
use crate::model::{JobDependencySpec, JobSpec, RetryPolicySpec};
use crate::pipeline::rules::RuleEvaluation;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ExecutableJob {
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

#[derive(Debug)]
pub struct ExecutionPlan {
    pub ordered: Vec<String>,
    pub nodes: HashMap<String, ExecutableJob>,
    pub dependents: HashMap<String, Vec<String>>,
    pub order_index: HashMap<String, usize>,
    pub variants: HashMap<String, Vec<JobVariantInfo>>,
}

impl ExecutionPlan {
    pub fn variants_for_dependency(&self, dep: &JobDependencySpec) -> Vec<String> {
        let Some(entries) = self.variants.get(&dep.job) else {
            return Vec::new();
        };
        select_variants(entries, dep)
            .into_iter()
            .map(|variant| variant.name.clone())
            .collect()
    }
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
