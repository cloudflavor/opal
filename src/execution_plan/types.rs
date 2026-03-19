use crate::compiler::{JobInstance, JobVariantInfo};
use crate::model::JobDependencySpec;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ExecutableJob {
    pub instance: JobInstance,
    pub log_path: PathBuf,
    pub log_hash: String,
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
