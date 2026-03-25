use crate::gitlab::{
    ArtifactConfig, ArtifactWhen, CacheConfig, CacheKey, CachePolicy, DependencySource,
    EnvironmentAction, EnvironmentConfig, ExternalDependency, Job, JobDependency, ParallelConfig,
    ParallelMatrixEntry, ParallelVariable, PipelineDefaults, PipelineFilters, RetryPolicy,
    ServiceConfig, WorkflowConfig,
};
use crate::model::{
    ArtifactSpec, ArtifactWhenSpec, CacheKeySpec, CachePolicySpec, CacheSpec, DependencySourceSpec,
    EnvironmentActionSpec, EnvironmentSpec, ExternalDependencySpec, JobDependencySpec, JobSpec,
    ParallelConfigSpec, ParallelMatrixEntrySpec, ParallelVariableSpec, PipelineDefaultsSpec,
    PipelineFilterSpec, RetryPolicySpec, ServiceSpec, WorkflowSpec,
};

impl From<&JobSpec> for Job {
    fn from(value: &JobSpec) -> Self {
        Self {
            name: value.name.clone(),
            stage: value.stage.clone(),
            commands: value.commands.clone(),
            needs: value.needs.iter().map(JobDependency::from).collect(),
            explicit_needs: value.explicit_needs,
            dependencies: value.dependencies.clone(),
            before_script: value.before_script.clone(),
            after_script: value.after_script.clone(),
            inherit_default_before_script: value.inherit_default_before_script,
            inherit_default_after_script: value.inherit_default_after_script,
            when: value.when.clone(),
            rules: value.rules.clone(),
            only: value.only.clone(),
            except: value.except.clone(),
            artifacts: ArtifactConfig {
                paths: value.artifacts.paths.clone(),
                exclude: value.artifacts.exclude.clone(),
                untracked: value.artifacts.untracked,
                when: ArtifactWhen::from(&value.artifacts.when),
                report_dotenv: value.artifacts.report_dotenv.clone(),
            },
            cache: value.cache.iter().map(CacheConfig::from).collect(),
            image: value.image.clone(),
            variables: value.variables.clone(),
            services: value.services.iter().map(ServiceConfig::from).collect(),
            timeout: value.timeout,
            retry: RetryPolicy::from(&value.retry),
            interruptible: value.interruptible,
            resource_group: value.resource_group.clone(),
            parallel: value.parallel.as_ref().map(ParallelConfig::from),
            tags: value.tags.clone(),
            environment: value.environment.as_ref().map(EnvironmentConfig::from),
        }
    }
}

impl From<&Job> for JobSpec {
    fn from(value: &Job) -> Self {
        Self {
            name: value.name.clone(),
            stage: value.stage.clone(),
            commands: value.commands.clone(),
            needs: value.needs.iter().map(JobDependencySpec::from).collect(),
            explicit_needs: value.explicit_needs,
            dependencies: value.dependencies.clone(),
            before_script: value.before_script.clone(),
            after_script: value.after_script.clone(),
            inherit_default_before_script: value.inherit_default_before_script,
            inherit_default_after_script: value.inherit_default_after_script,
            when: value.when.clone(),
            rules: value.rules.clone(),
            only: value.only.clone(),
            except: value.except.clone(),
            artifacts: ArtifactSpec {
                paths: value.artifacts.paths.clone(),
                exclude: value.artifacts.exclude.clone(),
                untracked: value.artifacts.untracked,
                when: ArtifactWhenSpec::from(&value.artifacts.when),
                report_dotenv: value.artifacts.report_dotenv.clone(),
            },
            cache: value.cache.iter().map(CacheSpec::from).collect(),
            image: value.image.clone(),
            variables: value.variables.clone(),
            services: value.services.iter().map(ServiceSpec::from).collect(),
            timeout: value.timeout,
            retry: RetryPolicySpec::from(&value.retry),
            interruptible: value.interruptible,
            resource_group: value.resource_group.clone(),
            parallel: value.parallel.as_ref().map(ParallelConfigSpec::from),
            tags: value.tags.clone(),
            environment: value.environment.as_ref().map(EnvironmentSpec::from),
        }
    }
}

impl From<&ArtifactWhen> for ArtifactWhenSpec {
    fn from(value: &ArtifactWhen) -> Self {
        match value {
            ArtifactWhen::OnSuccess => Self::OnSuccess,
            ArtifactWhen::OnFailure => Self::OnFailure,
            ArtifactWhen::Always => Self::Always,
        }
    }
}

impl From<&ArtifactWhenSpec> for ArtifactWhen {
    fn from(value: &ArtifactWhenSpec) -> Self {
        match value {
            ArtifactWhenSpec::OnSuccess => Self::OnSuccess,
            ArtifactWhenSpec::OnFailure => Self::OnFailure,
            ArtifactWhenSpec::Always => Self::Always,
        }
    }
}

impl From<&PipelineDefaults> for PipelineDefaultsSpec {
    fn from(value: &PipelineDefaults) -> Self {
        Self {
            image: value.image.clone(),
            before_script: value.before_script.clone(),
            after_script: value.after_script.clone(),
            variables: value.variables.clone(),
            cache: value.cache.iter().map(CacheSpec::from).collect(),
            services: value.services.iter().map(ServiceSpec::from).collect(),
            timeout: value.timeout,
            retry: RetryPolicySpec::from(&value.retry),
            interruptible: value.interruptible,
        }
    }
}

impl From<&WorkflowConfig> for WorkflowSpec {
    fn from(value: &WorkflowConfig) -> Self {
        Self {
            rules: value.rules.clone(),
        }
    }
}

impl From<&PipelineFilters> for PipelineFilterSpec {
    fn from(value: &PipelineFilters) -> Self {
        Self {
            only: value.only.clone(),
            except: value.except.clone(),
        }
    }
}

impl From<&JobDependency> for JobDependencySpec {
    fn from(value: &JobDependency) -> Self {
        Self {
            job: value.job.clone(),
            needs_artifacts: value.needs_artifacts,
            optional: value.optional,
            source: DependencySourceSpec::from(&value.source),
            parallel: value.parallel.clone(),
            inline_variant: value.inline_variant.clone(),
        }
    }
}

impl From<&JobDependencySpec> for JobDependency {
    fn from(value: &JobDependencySpec) -> Self {
        Self {
            job: value.job.clone(),
            needs_artifacts: value.needs_artifacts,
            optional: value.optional,
            source: DependencySource::from(&value.source),
            parallel: value.parallel.clone(),
            inline_variant: value.inline_variant.clone(),
        }
    }
}

impl From<&DependencySource> for DependencySourceSpec {
    fn from(value: &DependencySource) -> Self {
        match value {
            DependencySource::Local => Self::Local,
            DependencySource::External(ext) => Self::External(ExternalDependencySpec::from(ext)),
        }
    }
}

impl From<&DependencySourceSpec> for DependencySource {
    fn from(value: &DependencySourceSpec) -> Self {
        match value {
            DependencySourceSpec::Local => Self::Local,
            DependencySourceSpec::External(ext) => Self::External(ExternalDependency::from(ext)),
        }
    }
}

impl From<&ExternalDependency> for ExternalDependencySpec {
    fn from(value: &ExternalDependency) -> Self {
        Self {
            project: value.project.clone(),
            reference: value.reference.clone(),
        }
    }
}

impl From<&ExternalDependencySpec> for ExternalDependency {
    fn from(value: &ExternalDependencySpec) -> Self {
        Self {
            project: value.project.clone(),
            reference: value.reference.clone(),
        }
    }
}

impl From<&ServiceConfig> for ServiceSpec {
    fn from(value: &ServiceConfig) -> Self {
        Self {
            image: value.image.clone(),
            alias: value.alias.clone(),
            entrypoint: value.entrypoint.clone(),
            command: value.command.clone(),
            variables: value.variables.clone(),
        }
    }
}

impl From<&ServiceSpec> for ServiceConfig {
    fn from(value: &ServiceSpec) -> Self {
        Self {
            image: value.image.clone(),
            alias: value.alias.clone(),
            entrypoint: value.entrypoint.clone(),
            command: value.command.clone(),
            variables: value.variables.clone(),
        }
    }
}

impl From<&RetryPolicy> for RetryPolicySpec {
    fn from(value: &RetryPolicy) -> Self {
        Self {
            max: value.max,
            when: value.when.clone(),
            exit_codes: value.exit_codes.clone(),
        }
    }
}

impl From<&RetryPolicySpec> for RetryPolicy {
    fn from(value: &RetryPolicySpec) -> Self {
        Self {
            max: value.max,
            when: value.when.clone(),
            exit_codes: value.exit_codes.clone(),
        }
    }
}

impl From<&ParallelConfig> for ParallelConfigSpec {
    fn from(value: &ParallelConfig) -> Self {
        match value {
            ParallelConfig::Count(count) => Self::Count(*count),
            ParallelConfig::Matrix(entries) => {
                Self::Matrix(entries.iter().map(ParallelMatrixEntrySpec::from).collect())
            }
        }
    }
}

impl From<&ParallelConfigSpec> for ParallelConfig {
    fn from(value: &ParallelConfigSpec) -> Self {
        match value {
            ParallelConfigSpec::Count(count) => Self::Count(*count),
            ParallelConfigSpec::Matrix(entries) => {
                Self::Matrix(entries.iter().map(ParallelMatrixEntry::from).collect())
            }
        }
    }
}

impl From<&ParallelMatrixEntry> for ParallelMatrixEntrySpec {
    fn from(value: &ParallelMatrixEntry) -> Self {
        Self {
            variables: value
                .variables
                .iter()
                .map(ParallelVariableSpec::from)
                .collect(),
        }
    }
}

impl From<&ParallelMatrixEntrySpec> for ParallelMatrixEntry {
    fn from(value: &ParallelMatrixEntrySpec) -> Self {
        Self {
            variables: value.variables.iter().map(ParallelVariable::from).collect(),
        }
    }
}

impl From<&ParallelVariable> for ParallelVariableSpec {
    fn from(value: &ParallelVariable) -> Self {
        Self {
            name: value.name.clone(),
            values: value.values.clone(),
        }
    }
}

impl From<&ParallelVariableSpec> for ParallelVariable {
    fn from(value: &ParallelVariableSpec) -> Self {
        Self {
            name: value.name.clone(),
            values: value.values.clone(),
        }
    }
}

impl From<&EnvironmentConfig> for EnvironmentSpec {
    fn from(value: &EnvironmentConfig) -> Self {
        Self {
            name: value.name.clone(),
            url: value.url.clone(),
            on_stop: value.on_stop.clone(),
            auto_stop_in: value.auto_stop_in,
            action: EnvironmentActionSpec::from(value.action),
        }
    }
}

impl From<&EnvironmentSpec> for EnvironmentConfig {
    fn from(value: &EnvironmentSpec) -> Self {
        Self {
            name: value.name.clone(),
            url: value.url.clone(),
            on_stop: value.on_stop.clone(),
            auto_stop_in: value.auto_stop_in,
            action: EnvironmentAction::from(value.action),
        }
    }
}

impl From<EnvironmentAction> for EnvironmentActionSpec {
    fn from(value: EnvironmentAction) -> Self {
        match value {
            EnvironmentAction::Start => Self::Start,
            EnvironmentAction::Prepare => Self::Prepare,
            EnvironmentAction::Stop => Self::Stop,
            EnvironmentAction::Verify => Self::Verify,
            EnvironmentAction::Access => Self::Access,
        }
    }
}

impl From<EnvironmentActionSpec> for EnvironmentAction {
    fn from(value: EnvironmentActionSpec) -> Self {
        match value {
            EnvironmentActionSpec::Start => Self::Start,
            EnvironmentActionSpec::Prepare => Self::Prepare,
            EnvironmentActionSpec::Stop => Self::Stop,
            EnvironmentActionSpec::Verify => Self::Verify,
            EnvironmentActionSpec::Access => Self::Access,
        }
    }
}

impl From<&CacheConfig> for CacheSpec {
    fn from(value: &CacheConfig) -> Self {
        Self {
            key: match &value.key {
                CacheKey::Literal(raw) => CacheKeySpec::Literal(raw.clone()),
                CacheKey::Files { files, prefix } => CacheKeySpec::Files {
                    files: files.clone(),
                    prefix: prefix.clone(),
                },
            },
            fallback_keys: value.fallback_keys.clone(),
            paths: value.paths.clone(),
            policy: CachePolicySpec::from(value.policy),
        }
    }
}

impl From<&CacheSpec> for CacheConfig {
    fn from(value: &CacheSpec) -> Self {
        Self {
            key: match &value.key {
                CacheKeySpec::Literal(raw) => CacheKey::Literal(raw.clone()),
                CacheKeySpec::Files { files, prefix } => CacheKey::Files {
                    files: files.clone(),
                    prefix: prefix.clone(),
                },
            },
            fallback_keys: value.fallback_keys.clone(),
            paths: value.paths.clone(),
            policy: CachePolicy::from(value.policy),
        }
    }
}

impl From<CachePolicy> for CachePolicySpec {
    fn from(value: CachePolicy) -> Self {
        match value {
            CachePolicy::Pull => Self::Pull,
            CachePolicy::Push => Self::Push,
            CachePolicy::PullPush => Self::PullPush,
        }
    }
}

impl From<CachePolicySpec> for CachePolicy {
    fn from(value: CachePolicySpec) -> Self {
        match value {
            CachePolicySpec::Pull => Self::Pull,
            CachePolicySpec::Push => Self::Push,
            CachePolicySpec::PullPush => Self::PullPush,
        }
    }
}
