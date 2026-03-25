use crate::compiler::{JobInstance, JobVariantInfo};
use crate::model::JobDependencySpec;
use anyhow::{Result, anyhow};
use std::collections::{HashMap, HashSet, VecDeque};
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

    pub fn select_jobs(&self, selectors: &[String]) -> Result<Self> {
        if selectors.is_empty() {
            return Ok(self.clone());
        }

        let mut requested = HashSet::new();
        for selector in selectors {
            let matches = self.resolve_selector(selector);
            if matches.is_empty() {
                return Err(anyhow!(
                    "selected job '{}' was not found in execution plan",
                    selector
                ));
            }
            requested.extend(matches);
        }

        let mut keep = requested.clone();
        let mut queue: VecDeque<String> = requested.into_iter().collect();
        while let Some(name) = queue.pop_front() {
            let Some(planned) = self.nodes.get(&name) else {
                continue;
            };
            for dep in &planned.instance.dependencies {
                if keep.insert(dep.clone()) {
                    queue.push_back(dep.clone());
                }
            }
        }

        let ordered = self
            .ordered
            .iter()
            .filter(|name| keep.contains(*name))
            .cloned()
            .collect::<Vec<_>>();
        let nodes = self
            .nodes
            .iter()
            .filter(|(name, _)| keep.contains(*name))
            .map(|(name, planned)| (name.clone(), planned.clone()))
            .collect::<HashMap<_, _>>();
        let dependents = self
            .dependents
            .iter()
            .filter_map(|(name, downstream)| {
                if !keep.contains(name) {
                    return None;
                }
                let filtered = downstream
                    .iter()
                    .filter(|child| keep.contains(*child))
                    .cloned()
                    .collect::<Vec<_>>();
                Some((name.clone(), filtered))
            })
            .collect::<HashMap<_, _>>();
        let order_index = ordered
            .iter()
            .enumerate()
            .map(|(idx, name)| (name.clone(), idx))
            .collect::<HashMap<_, _>>();
        let variants = self
            .variants
            .iter()
            .filter_map(|(base, entries)| {
                let filtered = entries
                    .iter()
                    .filter(|entry| keep.contains(&entry.name))
                    .cloned()
                    .collect::<Vec<_>>();
                if filtered.is_empty() {
                    None
                } else {
                    Some((base.clone(), filtered))
                }
            })
            .collect::<HashMap<_, _>>();

        Ok(Self {
            ordered,
            nodes,
            dependents,
            order_index,
            variants,
        })
    }

    fn resolve_selector(&self, selector: &str) -> HashSet<String> {
        let mut matches = HashSet::new();
        if self.nodes.contains_key(selector) {
            matches.insert(selector.to_string());
        }
        if let Some(entries) = self.variants.get(selector) {
            matches.extend(entries.iter().map(|entry| entry.name.clone()));
        }
        matches
    }
}

fn select_variants<'a>(
    variants: &'a [JobVariantInfo],
    dep: &JobDependencySpec,
) -> Vec<&'a JobVariantInfo> {
    // TODO: this is the nth function like this, stop it, please
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

impl Clone for ExecutionPlan {
    fn clone(&self) -> Self {
        Self {
            ordered: self.ordered.clone(),
            nodes: self.nodes.clone(),
            dependents: self.dependents.clone(),
            order_index: self.order_index.clone(),
            variants: self.variants.clone(),
        }
    }
}
