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
                instance: compiled,
                log_path,
                log_hash,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{JobInstance, JobVariantInfo};
    use crate::model::{
        ArtifactSpec, DependencySourceSpec, JobDependencySpec, JobSpec, RetryPolicySpec,
    };
    use crate::pipeline::rules::RuleEvaluation;

    #[test]
    fn build_execution_plan_assigns_log_targets_and_preserves_runtime_metadata() {
        let compiled = CompiledPipeline {
            ordered: vec!["build".into()],
            jobs: HashMap::from([(
                "build".into(),
                JobInstance {
                    job: job("build"),
                    stage_name: "compile".into(),
                    dependencies: vec!["setup".into()],
                    rule: RuleEvaluation {
                        allow_failure: true,
                        ..RuleEvaluation::default()
                    },
                    timeout: Some(std::time::Duration::from_secs(30)),
                    retry: RetryPolicySpec {
                        max: 2,
                        when: vec!["runner_system_failure".into()],
                    },
                    interruptible: true,
                    resource_group: Some("builder".into()),
                },
            )]),
            dependents: HashMap::from([("setup".into(), vec!["build".into()])]),
            order_index: HashMap::from([("build".into(), 0)]),
            variants: HashMap::new(),
        };

        let plan = build_execution_plan(compiled, |job| {
            (
                PathBuf::from(format!("/tmp/{}.log", job.name)),
                format!("hash-{}", job.name),
            )
        })
        .expect("execution plan builds");

        let executable = plan.nodes.get("build").expect("job exists");
        assert_eq!(plan.ordered, vec!["build".to_string()]);
        assert_eq!(executable.instance.stage_name, "compile");
        assert_eq!(executable.instance.dependencies, vec!["setup".to_string()]);
        assert_eq!(executable.log_path, PathBuf::from("/tmp/build.log"));
        assert_eq!(executable.log_hash, "hash-build");
        assert!(executable.instance.rule.allow_failure);
        assert_eq!(
            executable.instance.timeout,
            Some(std::time::Duration::from_secs(30))
        );
        assert_eq!(executable.instance.retry.max, 2);
        assert!(executable.instance.interruptible);
        assert_eq!(
            executable.instance.resource_group.as_deref(),
            Some("builder")
        );
        assert_eq!(plan.dependents["setup"], vec!["build".to_string()]);
    }

    #[test]
    fn build_execution_plan_preserves_variant_lookup_for_dependencies() {
        let compiled = CompiledPipeline {
            ordered: vec!["build: [linux, release]".into()],
            jobs: HashMap::from([(
                "build: [linux, release]".into(),
                JobInstance {
                    job: job("build: [linux, release]"),
                    stage_name: "build".into(),
                    dependencies: Vec::new(),
                    rule: RuleEvaluation::default(),
                    timeout: None,
                    retry: RetryPolicySpec::default(),
                    interruptible: false,
                    resource_group: None,
                },
            )]),
            dependents: HashMap::new(),
            order_index: HashMap::from([("build: [linux, release]".into(), 0)]),
            variants: HashMap::from([(
                "build".into(),
                vec![JobVariantInfo {
                    name: "build: [linux, release]".into(),
                    labels: HashMap::from([
                        ("OS".into(), "linux".into()),
                        ("MODE".into(), "release".into()),
                    ]),
                    ordered_values: vec!["linux".into(), "release".into()],
                }],
            )]),
        };

        let plan = build_execution_plan(compiled, |job| {
            (
                PathBuf::from(format!("/tmp/{}.log", job.name)),
                "hash".into(),
            )
        })
        .expect("execution plan builds");
        let dep = JobDependencySpec {
            job: "build".into(),
            needs_artifacts: false,
            optional: false,
            source: DependencySourceSpec::Local,
            parallel: None,
            inline_variant: Some(vec!["linux".into(), "release".into()]),
        };

        assert_eq!(
            plan.variants_for_dependency(&dep),
            vec!["build: [linux, release]".to_string()]
        );
    }

    #[test]
    fn build_execution_plan_errors_when_order_references_missing_job() {
        let compiled = CompiledPipeline {
            ordered: vec!["missing".into()],
            jobs: HashMap::new(),
            dependents: HashMap::new(),
            order_index: HashMap::new(),
            variants: HashMap::new(),
        };

        let err = build_execution_plan(compiled, |_job| {
            (PathBuf::from("/tmp/unused.log"), "unused".into())
        })
        .expect_err("missing job should error");

        assert!(err.to_string().contains("compiled job 'missing' missing"));
    }

    fn job(name: &str) -> JobSpec {
        JobSpec {
            name: name.into(),
            stage: "build".into(),
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
