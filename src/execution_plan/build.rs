use crate::compiler::CompiledPipeline;
use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::model::JobSpec;
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::path::PathBuf;

pub fn build_execution_plan<F>(compiled: CompiledPipeline, mut log_info: F) -> Result<ExecutionPlan>
where
    F: FnMut(&JobSpec) -> (PathBuf, String),
{
    let CompiledPipeline {
        ordered,
        jobs,
        dependents,
        order_index,
        variants,
    } = compiled;
    let mut nodes = HashMap::new();
    for name in &ordered {
        let compiled = jobs
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("compiled job '{}' missing from output", name))?;
        let (log_path, log_hash) = log_info(&compiled.job);
        nodes.insert(
            name.clone(),
            ExecutableJob {
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
    Ok(ExecutionPlan {
        ordered,
        nodes,
        dependents,
        order_index,
        variants,
    })
}
