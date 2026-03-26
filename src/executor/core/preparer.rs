use super::ExecutorCore;
use crate::execution_plan::ExecutionPlan;
use crate::model::DependencySourceSpec;
use crate::model::{JobSpec, PipelineDefaultsSpec, ServiceSpec};
use crate::pipeline::{VolumeMount, mounts};
use crate::secrets::SecretsStore;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

pub(crate) struct PreparedJobRun {
    pub host_workdir: PathBuf,
    pub env_vars: Vec<(String, String)>,
    pub service_runtime: Option<crate::executor::services::ServiceRuntime>,
    pub mounts: Vec<VolumeMount>,
    pub job_image: String,
    pub script_path: PathBuf,
}

pub(super) fn prepare_job_run(
    exec: &ExecutorCore,
    plan: &ExecutionPlan,
    job: &JobSpec,
) -> Result<PreparedJobRun> {
    exec.artifacts.prepare_targets(job)?;
    let workspace = super::workspace::prepare_job_workspace(exec, job)?;
    let mut env_vars = exec.job_env(job);
    let cache_env: HashMap<String, String> = env_vars.iter().cloned().collect();
    let service_configs = selected_services(&exec.pipeline.defaults, job);
    let service_runtime = crate::executor::services::ServiceRuntime::start(
        exec.config.engine,
        &exec.run_id,
        &job.name,
        &service_configs,
        &env_vars,
        &exec.shared_env,
    )?;
    if let Some(runtime) = service_runtime.as_ref() {
        env_vars.extend(runtime.link_env().iter().cloned());
    }

    let completed_jobs = exec.completed_jobs();
    let mut mounts = mounts::collect_volume_mounts(mounts::VolumeMountContext {
        job,
        plan,
        pipeline: &exec.pipeline,
        workspace_root: &exec.config.workdir,
        artifacts: &exec.artifacts,
        cache: &exec.cache,
        cache_env: &cache_env,
        completed_jobs: &completed_jobs,
        session_dir: &exec.session_dir,
        container_root: &exec.container_workdir,
        external: exec.external_artifacts.as_ref(),
    })?;
    append_runtime_mounts(
        &mut mounts,
        exec.session_dir.clone(),
        exec.container_session_dir.clone(),
        &exec.secrets,
    );
    mounts.extend(workspace.mounts.clone());
    merge_dotenv_env(
        &mut env_vars,
        collect_dotenv_env(exec, plan, job, &completed_jobs)?,
    );

    let job_image = exec.resolve_job_image_with_env(job, Some(&cache_env))?;
    let mut script_commands = expanded_commands(&exec.pipeline.defaults, job);
    if let Some(runtime) = service_runtime.as_ref() {
        prepend_service_hosts(&mut script_commands, runtime.host_aliases());
    }
    let script_path = crate::executor::script::write_job_script(
        &exec.scripts_dir,
        &exec.container_workdir,
        job,
        &script_commands,
        exec.verbose_scripts,
    )?;

    Ok(PreparedJobRun {
        host_workdir: workspace.host_workdir,
        env_vars,
        service_runtime,
        mounts,
        job_image,
        script_path,
    })
}

fn collect_dotenv_env(
    exec: &ExecutorCore,
    plan: &ExecutionPlan,
    job: &JobSpec,
    completed_jobs: &HashMap<String, crate::model::ArtifactSourceOutcome>,
) -> Result<Vec<(String, String)>> {
    let mut vars = Vec::new();

    for dependency in &job.needs {
        if !dependency.needs_artifacts {
            continue;
        }
        match &dependency.source {
            DependencySourceSpec::Local => {
                let dep_job = exec.pipeline.jobs.get(&dependency.job);
                let Some(dep_job) = dep_job else {
                    continue;
                };
                for variant in plan.variants_for_dependency(dependency) {
                    let Some(planned) = plan.nodes.get(&variant) else {
                        continue;
                    };
                    if !planned
                        .instance
                        .job
                        .artifacts
                        .when
                        .includes(completed_jobs.get(&variant).copied())
                    {
                        continue;
                    }
                    if let Some(report) = &dep_job.artifacts.report_dotenv {
                        let path = exec.artifacts.job_artifact_host_path(&variant, report);
                        merge_dotenv_env(&mut vars, crate::secrets::load_dotenv_env_pairs(&path)?);
                    }
                }
            }
            DependencySourceSpec::External(_) => {}
        }
    }

    for dep_name in &job.dependencies {
        let dep_job = exec.pipeline.jobs.get(dep_name).or_else(|| {
            plan.nodes
                .get(dep_name)
                .map(|planned| &planned.instance.job)
        });
        let Some(dep_job) = dep_job else {
            continue;
        };
        if !dep_job
            .artifacts
            .when
            .includes(completed_jobs.get(dep_name).copied())
        {
            continue;
        }
        if let Some(report) = &dep_job.artifacts.report_dotenv {
            let path = exec.artifacts.job_artifact_host_path(&dep_job.name, report);
            merge_dotenv_env(&mut vars, crate::secrets::load_dotenv_env_pairs(&path)?);
        }
    }

    Ok(vars)
}

fn merge_dotenv_env(env: &mut Vec<(String, String)>, extra: Vec<(String, String)>) {
    for (key, value) in extra {
        if let Some((_, existing)) = env
            .iter_mut()
            .find(|(existing_key, _)| existing_key == &key)
        {
            *existing = value;
        } else {
            env.push((key, value));
        }
    }
}

fn prepend_service_hosts(commands: &mut Vec<String>, aliases: &[(String, String)]) {
    if aliases.is_empty() {
        return;
    }
    let mut prefix = Vec::with_capacity(aliases.len());
    for (alias, ip) in aliases {
        prefix.push(format!(
            "printf '%s\\t%s\\n' '{}' '{}' >> /etc/hosts",
            ip, alias
        ));
    }
    prefix.append(commands);
    *commands = prefix;
}

fn expanded_commands(defaults: &PipelineDefaultsSpec, job: &JobSpec) -> Vec<String> {
    let mut cmds = Vec::new();
    if job.inherit_default_before_script && job.before_script.is_none() {
        cmds.extend(defaults.before_script.iter().cloned());
    }
    if let Some(custom) = &job.before_script {
        cmds.extend(custom.iter().cloned());
    }
    cmds.extend(job.commands.iter().cloned());
    if let Some(custom) = &job.after_script {
        cmds.extend(custom.iter().cloned());
    }
    if job.inherit_default_after_script && job.after_script.is_none() {
        cmds.extend(defaults.after_script.iter().cloned());
    }
    cmds
}

fn selected_services(defaults: &PipelineDefaultsSpec, job: &JobSpec) -> Vec<ServiceSpec> {
    if job.services.is_empty() {
        defaults.services.clone()
    } else {
        job.services.clone()
    }
}

fn append_runtime_mounts(
    mounts: &mut Vec<VolumeMount>,
    session_dir: PathBuf,
    container_session_dir: PathBuf,
    secrets: &SecretsStore,
) {
    mounts.push(VolumeMount {
        host: session_dir,
        container: container_session_dir,
        read_only: false,
    });
    if let Some((host, container_path)) = secrets.volume_mount() {
        mounts.push(VolumeMount {
            host,
            container: container_path,
            read_only: true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{append_runtime_mounts, expanded_commands, selected_services};
    use crate::gitlab::rules::JobRule;
    use crate::model::{
        ArtifactSpec, CacheKeySpec, CachePolicySpec, CacheSpec, DependencySourceSpec,
        JobDependencySpec, JobSpec, PipelineDefaultsSpec, RetryPolicySpec, ServiceSpec,
    };
    use crate::pipeline::VolumeMount;
    use crate::secrets::SecretsStore;
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn expanded_commands_job_hooks_override_defaults() {
        let defaults = PipelineDefaultsSpec {
            before_script: vec!["default-before".into()],
            after_script: vec!["default-after".into()],
            ..pipeline_defaults()
        };
        let job = JobSpec {
            before_script: Some(vec!["job-before".into()]),
            after_script: Some(vec!["job-after".into()]),
            commands: vec!["main".into()],
            ..job("build")
        };

        let commands = expanded_commands(&defaults, &job);

        assert_eq!(commands, vec!["job-before", "main", "job-after"]);
    }

    #[test]
    fn expanded_commands_uses_defaults_when_job_hooks_are_missing() {
        let defaults = PipelineDefaultsSpec {
            before_script: vec!["default-before".into()],
            after_script: vec!["default-after".into()],
            ..pipeline_defaults()
        };
        let job = JobSpec {
            before_script: None,
            after_script: None,
            commands: vec!["main".into()],
            ..job("build")
        };

        let commands = expanded_commands(&defaults, &job);

        assert_eq!(commands, vec!["default-before", "main", "default-after"]);
    }

    #[test]
    fn expanded_commands_empty_job_hooks_disable_default_hooks() {
        let defaults = PipelineDefaultsSpec {
            before_script: vec!["default-before".into()],
            after_script: vec!["default-after".into()],
            ..pipeline_defaults()
        };
        let job = JobSpec {
            before_script: Some(Vec::new()),
            after_script: Some(Vec::new()),
            commands: vec!["main".into()],
            ..job("build")
        };

        let commands = expanded_commands(&defaults, &job);

        assert_eq!(commands, vec!["main"]);
    }

    #[test]
    fn selected_services_prefers_job_services_over_defaults() {
        let defaults = PipelineDefaultsSpec {
            services: vec![service("redis:7", Some("redis"))],
            ..pipeline_defaults()
        };
        let no_job_services = job("build");
        let with_job_services = JobSpec {
            services: vec![service("postgres:16", Some("db"))],
            ..job("test")
        };

        let inherited = selected_services(&defaults, &no_job_services);
        let overridden = selected_services(&defaults, &with_job_services);

        assert_eq!(inherited[0].image, "redis:7");
        assert_eq!(overridden[0].image, "postgres:16");
    }

    #[test]
    fn append_runtime_mounts_adds_session_and_secret_mounts() {
        let temp_root = temp_path("preparer-secret-mounts");
        let secrets_root = temp_root.join(".opal").join("env");
        fs::create_dir_all(&secrets_root).expect("create secrets dir");
        fs::write(secrets_root.join("API_TOKEN"), "super-secret").expect("write secret");
        let secrets = SecretsStore::load(&temp_root).expect("load secrets");
        let mut mounts = vec![VolumeMount {
            host: PathBuf::from("/host/workdir"),
            container: PathBuf::from("/builds/project"),
            read_only: false,
        }];

        append_runtime_mounts(
            &mut mounts,
            PathBuf::from("/tmp/session"),
            PathBuf::from("/opal/run"),
            &secrets,
        );

        assert_eq!(mounts.len(), 3);
        assert_eq!(mounts[1].host, PathBuf::from("/tmp/session"));
        assert_eq!(mounts[1].container, PathBuf::from("/opal/run"));
        assert!(!mounts[1].read_only);
        assert_eq!(mounts[2].host, secrets_root);
        assert_eq!(mounts[2].container, PathBuf::from("/opal/secrets"));
        assert!(mounts[2].read_only);

        let _ = fs::remove_dir_all(temp_root);
    }

    fn temp_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("opal-{prefix}-{nanos}"))
    }

    fn pipeline_defaults() -> PipelineDefaultsSpec {
        PipelineDefaultsSpec {
            image: None,
            before_script: Vec::new(),
            after_script: Vec::new(),
            variables: HashMap::new(),
            cache: Vec::new(),
            services: Vec::new(),
            timeout: None,
            retry: RetryPolicySpec::default(),
            interruptible: false,
        }
    }

    fn job(name: &str) -> JobSpec {
        JobSpec {
            name: name.into(),
            stage: "test".into(),
            commands: vec!["echo ok".into()],
            needs: vec![JobDependencySpec {
                job: "setup".into(),
                needs_artifacts: false,
                optional: false,
                source: DependencySourceSpec::Local,
                parallel: None,
                inline_variant: None,
            }],
            explicit_needs: true,
            dependencies: Vec::new(),
            before_script: None,
            after_script: None,
            inherit_default_before_script: true,
            inherit_default_after_script: true,
            when: None,
            rules: Vec::<JobRule>::new(),
            only: Vec::new(),
            except: Vec::new(),
            artifacts: ArtifactSpec::default(),
            cache: vec![CacheSpec {
                key: CacheKeySpec::Literal("cache".into()),
                fallback_keys: Vec::new(),
                paths: vec![Path::new("target").to_path_buf()],
                policy: CachePolicySpec::PullPush,
            }],
            image: None,
            variables: HashMap::new(),
            services: Vec::new(),
            timeout: Some(Duration::from_secs(60)),
            retry: RetryPolicySpec::default(),
            interruptible: false,
            resource_group: None,
            parallel: None,
            tags: Vec::new(),
            environment: None,
        }
    }

    fn service(image: &str, alias: Option<&str>) -> ServiceSpec {
        ServiceSpec {
            image: image.into(),
            aliases: alias.into_iter().map(str::to_string).collect(),
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        }
    }
}
