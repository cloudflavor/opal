use super::ExecutorCore;
use crate::EngineKind;
use crate::app::context::{resolve_engine, validate_engine_choice};
use crate::config::ResolvedJobOverride;
use crate::env::expand_value;
use crate::execution_plan::ExecutionPlan;
use crate::executor::sandbox::{
    ResolvedSandboxRuntime, prepare_job_env, resolve_runtime as resolve_sandbox_runtime,
};
use crate::executor::script::write_job_script;
use crate::executor::services::ServiceRuntime;
use crate::model::ArtifactSourceOutcome;
use crate::model::DependencySourceSpec;
use crate::model::{ImageSpec, JobSpec, PipelineDefaultsSpec, ServiceSpec};
use crate::pipeline::{VolumeMount, mounts};
use crate::secrets::{SecretsStore, load_dotenv_env_pairs};
use anyhow::{Result, anyhow};
use std::collections::HashMap;
#[cfg(not(unix))]
use std::fs as std_fs;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use tokio::fs as tokio_fs;

pub(crate) struct PreparedJobRun {
    pub host_workdir: PathBuf,
    pub env_vars: Vec<(String, String)>,
    pub job_engine: EngineKind,
    pub sandbox_settings_path: Option<PathBuf>,
    pub sandbox_debug: bool,
    pub service_runtime: Option<ServiceRuntime>,
    pub mounts: Vec<VolumeMount>,
    pub job_image: ImageSpec,
    pub arch: Option<String>,
    pub privileged: bool,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub script_path: PathBuf,
}

pub(super) async fn prepare_job_run(
    exec: &ExecutorCore,
    plan: &ExecutionPlan,
    job: &JobSpec,
) -> Result<PreparedJobRun> {
    exec.artifacts.prepare_targets(job).await?;
    let workspace = super::workspace::prepare_job_workspace(exec, job)?;
    let mut env_vars = exec.job_env(job);
    let job_override = exec.config.settings.job_override_for(&job.name);
    let job_engine = resolved_job_engine(exec.config.engine, job_override.as_ref())?;
    if matches!(job_engine, EngineKind::Sandbox) {
        prepare_job_env(
            &exec.container_workdir,
            exec.shared_env.get("CI_PROJECT_DIR").map(String::as_str),
            &workspace.host_workdir,
            &mut env_vars,
        )
        .await?;
    }
    let cache_env: HashMap<String, String> = env_vars.iter().cloned().collect();
    let service_configs = selected_services(
        &exec.pipeline.defaults,
        job,
        &cache_env,
        exec.config.settings.map_host_user(),
    );
    if matches!(job_engine, EngineKind::Sandbox) && !service_configs.is_empty() {
        return Err(anyhow!(
            "job '{}' uses the sandbox engine with services, but services run on the global engine and are not supported with sandbox job execution yet",
            job.name
        ));
    }
    let service_runtime = ServiceRuntime::start(
        exec.config.engine,
        &exec.run_id,
        &job.name,
        &service_configs,
        &env_vars,
        exec.config.settings.preserve_runtime_objects(),
        &exec.shared_env,
    )
    .await?;
    if let Some(runtime) = service_runtime.as_ref() {
        env_vars.extend(runtime.link_env().iter().cloned());
    }
    let sandbox_runtime = if matches!(job_engine, EngineKind::Sandbox) {
        resolve_sandbox_runtime(
            &exec.session_dir,
            exec.config.settings.sandbox_settings(),
            job,
            job_override.as_ref(),
        )
        .await?
    } else {
        ResolvedSandboxRuntime::default()
    };

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
    mounts.extend(exec.bootstrap_mounts.clone());
    mounts.extend(workspace.mounts.clone());
    if matches!(job_engine, EngineKind::Sandbox) {
        materialize_sandbox_mounts(&workspace.host_workdir, &exec.container_workdir, &mounts)
            .await?;
        rewrite_env_mount_prefixes(&mut env_vars, &mounts);
    }
    merge_dotenv_env(
        &mut env_vars,
        collect_dotenv_env(exec, plan, job, &completed_jobs)?,
    );

    let job_image = exec.resolve_job_image_with_env(job, Some(&cache_env))?;
    let mut script_commands = expanded_commands(&exec.pipeline.defaults, job);
    if let Some(runtime) = service_runtime.as_ref()
        && matches!(job_engine, EngineKind::ContainerCli)
    {
        prepend_service_hosts(&mut script_commands, runtime.host_aliases());
    }
    let script_workdir = if matches!(job_engine, EngineKind::Sandbox) {
        workspace.host_workdir.as_path()
    } else {
        exec.container_workdir.as_path()
    };
    let script_path = write_job_script(
        &exec.scripts_dir,
        script_workdir,
        job,
        &script_commands,
        exec.verbose_scripts,
    )?;

    Ok(PreparedJobRun {
        host_workdir: workspace.host_workdir,
        env_vars,
        job_engine,
        sandbox_settings_path: sandbox_runtime.settings_path,
        sandbox_debug: sandbox_runtime.debug,
        service_runtime,
        mounts,
        job_image,
        arch: job_override.as_ref().and_then(|cfg| cfg.arch.clone()),
        privileged: job_override
            .as_ref()
            .and_then(|cfg| cfg.privileged)
            .unwrap_or(false),
        cap_add: job_override
            .as_ref()
            .map(|cfg| cfg.cap_add.clone())
            .unwrap_or_default(),
        cap_drop: job_override
            .as_ref()
            .map(|cfg| cfg.cap_drop.clone())
            .unwrap_or_default(),
        script_path,
    })
}

fn resolved_job_engine(
    default_engine: EngineKind,
    override_cfg: Option<&ResolvedJobOverride>,
) -> Result<EngineKind> {
    let Some(choice) = override_cfg.and_then(|cfg| cfg.engine) else {
        return Ok(default_engine);
    };
    validate_engine_choice(choice)?;
    Ok(resolve_engine(choice))
}

fn collect_dotenv_env(
    exec: &ExecutorCore,
    plan: &ExecutionPlan,
    job: &JobSpec,
    completed_jobs: &HashMap<String, ArtifactSourceOutcome>,
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
                        merge_dotenv_env(&mut vars, load_dotenv_env_pairs(&path)?);
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
            merge_dotenv_env(&mut vars, load_dotenv_env_pairs(&path)?);
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

fn rewrite_env_prefix(env: &mut [(String, String)], from: &str, to: &str) {
    for (_, value) in env.iter_mut() {
        if value == from {
            *value = to.to_string();
            continue;
        }
        if let Some(suffix) = value.strip_prefix(from) {
            *value = format!("{to}{suffix}");
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

fn selected_services(
    defaults: &PipelineDefaultsSpec,
    job: &JobSpec,
    env_lookup: &HashMap<String, String>,
    map_host_user: bool,
) -> Vec<ServiceSpec> {
    if job.services.is_empty() {
        if !job.inherit_default_services {
            return Vec::new();
        }
        expand_service_images(defaults.services.clone(), env_lookup, map_host_user)
    } else {
        expand_service_images(job.services.clone(), env_lookup, map_host_user)
    }
}

fn expand_service_images(
    mut services: Vec<ServiceSpec>,
    env_lookup: &HashMap<String, String>,
    map_host_user: bool,
) -> Vec<ServiceSpec> {
    let mapped_user = map_host_user
        .then(|| host_user_from_lookup(env_lookup))
        .flatten();
    for service in &mut services {
        if service.image.contains('$') {
            service.image = expand_value(&service.image, env_lookup);
        }
        if let Some(user) = service
            .docker_user
            .as_ref()
            .filter(|value| value.contains('$'))
        {
            service.docker_user = Some(expand_value(user, env_lookup));
        }
        if service.docker_user.is_none()
            && let Some(user) = mapped_user.as_ref()
        {
            service.docker_user = Some(user.clone());
        }
    }
    services
}

fn host_user_from_lookup(lookup: &HashMap<String, String>) -> Option<String> {
    let uid = lookup.get("OPAL_HOST_UID").map(String::as_str)?.trim();
    let gid = lookup.get("OPAL_HOST_GID").map(String::as_str)?.trim();
    if uid.is_empty() || gid.is_empty() {
        return None;
    }
    Some(format!("{uid}:{gid}"))
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

fn rewrite_env_mount_prefixes(env: &mut [(String, String)], mounts: &[VolumeMount]) {
    for mount in mounts {
        rewrite_env_prefix(
            env,
            &mount.container.display().to_string(),
            &mount.host.display().to_string(),
        );
    }
}

async fn materialize_sandbox_mounts(
    workspace_root: &Path,
    container_root: &Path,
    mounts: &[VolumeMount],
) -> Result<()> {
    for mount in mounts {
        let Ok(relative) = mount.container.strip_prefix(container_root) else {
            continue;
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        if !tokio_fs::try_exists(&mount.host).await.unwrap_or(false) {
            continue;
        }
        let target = workspace_root.join(relative);
        if let Some(parent) = target.parent() {
            tokio_fs::create_dir_all(parent).await?;
        }
        remove_existing_target(&target).await?;
        if mount.read_only {
            copy_path_recursive(&mount.host, &target).await?;
        } else {
            link_mount_target(&mount.host, &target)?;
        }
    }
    Ok(())
}

async fn remove_existing_target(target: &Path) -> Result<()> {
    let metadata = match tokio_fs::symlink_metadata(target).await {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        tokio_fs::remove_file(target).await?;
    } else {
        tokio_fs::remove_dir_all(target).await?;
    }
    Ok(())
}

fn link_mount_target(source: &Path, target: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        unix_fs::symlink(source, target)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        if source.is_dir() {
            return Err(anyhow!(
                "sandbox writable mount materialization for directories requires symlink support"
            ));
        }
        std_fs::copy(source, target)?;
        Ok(())
    }
}

async fn copy_path_recursive(src: &Path, dest: &Path) -> Result<()> {
    let metadata = tokio_fs::symlink_metadata(src).await?;
    if metadata.file_type().is_symlink() {
        let target = tokio_fs::read_link(src).await?;
        link_mount_target(&target, dest)?;
        return Ok(());
    }
    if metadata.is_dir() {
        tokio_fs::create_dir_all(dest).await?;
        let mut read_dir = tokio_fs::read_dir(src).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let child_src = entry.path();
            let child_dest = dest.join(entry.file_name());
            Box::pin(copy_path_recursive(&child_src, &child_dest)).await?;
        }
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        tokio_fs::create_dir_all(parent).await?;
    }
    tokio_fs::copy(src, dest).await?;
    tokio_fs::set_permissions(dest, metadata.permissions()).await?;
    Ok(())
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

        let inherited = selected_services(&defaults, &no_job_services, &HashMap::new(), false);
        let overridden = selected_services(&defaults, &with_job_services, &HashMap::new(), false);

        assert_eq!(inherited[0].image, "redis:7");
        assert_eq!(overridden[0].image, "postgres:16");
    }

    #[test]
    fn selected_services_expands_image_variables() {
        let defaults = PipelineDefaultsSpec {
            services: vec![service("${REGISTRY_HOST}/redis:7", Some("redis"))],
            ..pipeline_defaults()
        };
        let job = job("build");

        let services = selected_services(
            &defaults,
            &job,
            &HashMap::from([("REGISTRY_HOST".into(), "docker.io/library".into())]),
            false,
        );

        assert_eq!(services[0].image, "docker.io/library/redis:7");
    }

    #[test]
    fn selected_services_expands_docker_user_variables() {
        let defaults = PipelineDefaultsSpec {
            services: vec![ServiceSpec {
                docker_user: Some("${OPAL_HOST_UID}:${OPAL_HOST_GID}".into()),
                ..service("redis:7", Some("redis"))
            }],
            ..pipeline_defaults()
        };
        let job = job("build");

        let services = selected_services(
            &defaults,
            &job,
            &HashMap::from([
                ("OPAL_HOST_UID".into(), "1000".into()),
                ("OPAL_HOST_GID".into(), "1001".into()),
            ]),
            false,
        );

        assert_eq!(services[0].docker_user.as_deref(), Some("1000:1001"));
    }

    #[test]
    fn selected_services_maps_host_user_when_enabled_and_unset() {
        let defaults = PipelineDefaultsSpec {
            services: vec![service("redis:7", Some("redis"))],
            ..pipeline_defaults()
        };
        let job = job("build");

        let services = selected_services(
            &defaults,
            &job,
            &HashMap::from([
                ("OPAL_HOST_UID".into(), "501".into()),
                ("OPAL_HOST_GID".into(), "20".into()),
            ]),
            true,
        );

        assert_eq!(services[0].docker_user.as_deref(), Some("501:20"));
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
            inherit_default_image: true,
            inherit_default_cache: true,
            inherit_default_services: true,
            inherit_default_timeout: true,
            inherit_default_retry: true,
            inherit_default_interruptible: true,
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
            docker_platform: None,
            docker_user: None,
            entrypoint: Vec::new(),
            command: Vec::new(),
            variables: HashMap::new(),
        }
    }
}
