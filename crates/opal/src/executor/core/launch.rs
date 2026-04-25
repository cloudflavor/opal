use super::ExecutorCore;
use crate::app::context::resolve_engine;
use crate::display::{self, indent_block};
use crate::env::expand_value;
use crate::execution_plan::ExecutableJob;
use crate::model::{ImageSpec, JobSpec};
use crate::naming::{job_name_slug, stage_name_slug};
use crate::pipeline::JobRunInfo;
use crate::ui::UiBridge;
use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;

const MAX_CONTAINER_NAME: usize = 63;

pub(super) fn log_job_start(
    exec: &ExecutorCore,
    planned: &ExecutableJob,
    ui: Option<&UiBridge>,
) -> Result<JobRunInfo> {
    let attempt = exec.runtime_state.next_attempt(&planned.instance.job.name);
    let container_name = job_container_name(
        &exec.run_id,
        &planned.instance.stage_name,
        &planned.instance.job,
        attempt,
    );
    if let Some(ui) = ui {
        ui.job_started(&planned.instance.job.name);
    }

    if exec.live_console_output_enabled() {
        let display = exec.display();
        if exec.stage_tracker.start(&planned.instance.stage_name) {
            if exec.stage_tracker.position(&planned.instance.stage_name) > 0 {
                display::print_blank_line();
            }
            display::print_line(display.stage_header(&planned.instance.stage_name));
        }

        let job = &planned.instance.job;
        let job_label = display.bold_green("  job:");
        let job_name = display.bold_white(job.name.as_str());
        display::print_line(format!("{} {}", job_label, job_name));

        if let Some(needs) = display.format_needs(job) {
            let needs_label = display.bold_cyan("    needs:");
            display::print_line(format!("{} {}", needs_label, needs));
        }
        if let Some(paths) = display.format_paths(&job.artifacts.paths) {
            let artifacts_label = display.bold_cyan("    artifacts:");
            display::print_line(format!("{} {}", artifacts_label, paths));
        }

        let job_image = resolve_job_image(exec, job)?;
        let image_label = display.bold_cyan("    image:");
        display::print_line(format!(
            "{} {}",
            image_label,
            display::format_image_spec(&job_image)
        ));

        let container_label = display.bold_cyan("    container:");
        display::print_line(format!("{} {}", container_label, container_name));

        if exec.verbose_scripts && !job.commands.is_empty() {
            let script_label = display.bold_yellow("    script:");
            display::print_line(format!(
                "{}\n{}",
                script_label,
                indent_block(&job.commands.join("\n"), "      │ ")
            ));
        }
    }

    let job_override = exec
        .config
        .settings
        .job_override_for(&planned.instance.job.name);
    let job_engine = job_override
        .as_ref()
        .and_then(|cfg| cfg.engine)
        .map(resolve_engine)
        .unwrap_or(exec.config.engine);
    if !matches!(job_engine, crate::EngineKind::Sandbox) {
        exec.runtime_state.track_running_container(
            &planned.instance.job.name,
            &container_name,
            job_engine,
        );
    }
    Ok(JobRunInfo { container_name })
}

pub(super) fn resolve_job_image(exec: &ExecutorCore, job: &JobSpec) -> Result<ImageSpec> {
    resolve_job_image_with_env(exec, job, None)
}

pub(super) fn resolve_job_image_with_env(
    exec: &ExecutorCore,
    job: &JobSpec,
    env_lookup: Option<&HashMap<String, String>>,
) -> Result<ImageSpec> {
    let mut image = if let Some(image) = job.image.as_ref() {
        image.clone()
    } else if job.inherit_default_image {
        if let Some(image) = exec.pipeline.defaults.image.as_ref() {
            image.clone()
        } else if let Some(image) = exec.config.image.clone() {
            ImageSpec {
                name: image,
                docker_platform: None,
                docker_user: None,
                entrypoint: Vec::new(),
            }
        } else {
            return Err(anyhow!(
                "job '{}' has no image (use --base-image or set image in pipeline/job)",
                job.name
            ));
        }
    } else if let Some(image) = exec.config.image.clone() {
        ImageSpec {
            name: image,
            docker_platform: None,
            docker_user: None,
            entrypoint: Vec::new(),
        }
    } else {
        return Err(anyhow!(
            "job '{}' has no image (use --base-image or set image in pipeline/job)",
            job.name
        ));
    };

    let owned_lookup;
    let lookup = if let Some(map) = env_lookup {
        map
    } else {
        owned_lookup = exec.job_env(job).into_iter().collect();
        &owned_lookup
    };

    if image.name.contains('$') {
        image.name = expand_value(&image.name, lookup);
    }
    if let Some(user) = image
        .docker_user
        .as_ref()
        .filter(|value| value.contains('$'))
    {
        image.docker_user = Some(expand_value(user, lookup));
    }
    if image.docker_user.is_none()
        && exec.config.settings.map_host_user()
        && let Some(mapped_user) = host_user_from_lookup(lookup)
    {
        image.docker_user = Some(mapped_user);
    }

    Ok(image)
}

fn host_user_from_lookup(lookup: &HashMap<String, String>) -> Option<String> {
    let uid = lookup.get("OPAL_HOST_UID").map(String::as_str)?.trim();
    let gid = lookup.get("OPAL_HOST_GID").map(String::as_str)?.trim();
    if uid.is_empty() || gid.is_empty() {
        return None;
    }
    Some(format!("{uid}:{gid}"))
}

fn job_container_name(run_id: &str, stage_name: &str, job: &JobSpec, attempt: usize) -> String {
    let base = format!(
        "opal-{}-{}-{}-{:02}",
        run_id,
        stage_name_slug(stage_name),
        job_name_slug(&job.name),
        attempt
    );
    if base.len() <= MAX_CONTAINER_NAME {
        return base;
    }
    short_container_name(run_id, stage_name, job, attempt)
}

fn short_container_name(run_id: &str, stage_name: &str, job: &JobSpec, attempt: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run_id.as_bytes());
    hasher.update(stage_name.as_bytes());
    hasher.update(job.name.as_bytes());
    let digest = hasher.finalize();
    let mut short = String::with_capacity(16);
    for byte in digest.iter().take(6) {
        let _ = FmtWrite::write_fmt(&mut short, format_args!("{:02x}", byte));
    }
    format!("opal-{short}-{:02}", attempt)
}

#[cfg(test)]
mod tests {
    use super::{resolve_job_image_with_env, short_container_name};
    use crate::config::OpalConfig;
    use crate::executor::core::ExecutorCore;
    use crate::model::{
        ArtifactSpec, JobSpec, PipelineDefaultsSpec, PipelineSpec, RetryPolicySpec, StageSpec,
    };
    use crate::{EngineKind, ExecutorConfig};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    #[test]
    fn short_container_name_stays_within_runtime_limit() {
        let job = job("really-long-job-name-that-keeps-going-to-force-a-shortened-container-name");

        let name = short_container_name(
            "very-long-run-id-for-testing",
            "very-long-stage-name",
            &job,
            1,
        );

        assert!(name.len() <= 63);
        assert!(name.starts_with("opal-"));
        assert!(name.ends_with("-01"));
    }

    #[test]
    fn resolve_job_image_expands_variables_from_lookup() {
        let exec = test_core();
        let job = JobSpec {
            image: Some("registry.example.com/${TARGET}:latest".into()),
            ..job("build")
        };

        let image = resolve_job_image_with_env(
            &exec,
            &job,
            Some(&HashMap::from([("TARGET".into(), "linux".into())])),
        )
        .expect("image resolves");

        assert_eq!(image.name, "registry.example.com/linux:latest");
    }

    #[test]
    fn resolve_job_image_expands_docker_user_from_lookup() {
        let exec = test_core();
        let job = JobSpec {
            image: Some(crate::model::ImageSpec {
                name: "alpine:3.20".into(),
                docker_platform: None,
                docker_user: Some("${OPAL_HOST_UID}:${OPAL_HOST_GID}".into()),
                entrypoint: Vec::new(),
            }),
            ..job("build")
        };

        let image = resolve_job_image_with_env(
            &exec,
            &job,
            Some(&HashMap::from([
                ("OPAL_HOST_UID".into(), "1000".into()),
                ("OPAL_HOST_GID".into(), "1001".into()),
            ])),
        )
        .expect("image resolves");

        assert_eq!(image.docker_user.as_deref(), Some("1000:1001"));
    }

    #[test]
    fn resolve_job_image_maps_host_user_when_enabled_and_unset() {
        let mut exec = test_core();
        exec.config.settings = toml::from_str(
            r#"
[engine]
map_host_user = true
"#,
        )
        .expect("parse config");
        let job = JobSpec {
            image: Some("alpine:3.20".into()),
            ..job("build")
        };

        let image = resolve_job_image_with_env(
            &exec,
            &job,
            Some(&HashMap::from([
                ("OPAL_HOST_UID".into(), "501".into()),
                ("OPAL_HOST_GID".into(), "20".into()),
            ])),
        )
        .expect("image resolves");

        assert_eq!(image.docker_user.as_deref(), Some("501:20"));
    }

    #[test]
    fn live_console_output_is_disabled_when_tui_is_enabled() {
        let mut exec = test_core();
        exec.config.enable_tui = true;
        exec.config.emit_console_output = true;

        assert!(!exec.live_console_output_enabled());
    }

    #[test]
    fn live_console_output_remains_enabled_for_no_tui_runs() {
        let mut exec = test_core();
        exec.config.enable_tui = false;
        exec.config.emit_console_output = true;

        assert!(exec.live_console_output_enabled());
    }

    fn test_core() -> ExecutorCore {
        ExecutorCore {
            config: ExecutorConfig {
                pipeline: PathBuf::from("pipelines/tests/filters.gitlab-ci.yml"),
                workdir: PathBuf::from("."),
                image: None,
                env_includes: Vec::new(),
                selected_jobs: Vec::new(),
                max_parallel_jobs: 1,
                enable_tui: false,
                emit_console_output: false,
                engine: EngineKind::Docker,
                gitlab: None,
                settings: OpalConfig::default(),
                trace_scripts: false,
            },
            pipeline: PipelineSpec {
                stages: vec![StageSpec {
                    name: "test".into(),
                    jobs: vec!["build".into()],
                }],
                jobs: HashMap::new(),
                defaults: PipelineDefaultsSpec {
                    image: Some("docker.io/library/alpine:3.19".into()),
                    before_script: Vec::new(),
                    after_script: Vec::new(),
                    variables: HashMap::new(),
                    cache: Vec::new(),
                    services: Vec::new(),
                    timeout: None,
                    retry: RetryPolicySpec::default(),
                    interruptible: false,
                },
                workflow: None,
                filters: Default::default(),
            },
            use_color: false,
            scripts_dir: PathBuf::from("/tmp/scripts"),
            logs_dir: PathBuf::from("/tmp/logs"),
            session_dir: PathBuf::from("/tmp/session"),
            container_session_dir: Path::new("/opal").join("run"),
            run_id: "run-123".into(),
            verbose_scripts: false,
            env_vars: Vec::new(),
            shared_env: HashMap::new(),
            container_workdir: Path::new(crate::executor::core::CONTAINER_ROOT).join("opal"),
            stage_tracker: crate::executor::core::stage_tracker::StageTracker::new(&[(
                "test".into(),
                1,
            )]),
            runtime_state: Default::default(),
            history_store: crate::executor::core::history_store::HistoryStore::load(
                std::env::temp_dir().join("opal-launch-history.json"),
            ),
            secrets: Default::default(),
            artifacts: crate::pipeline::ArtifactManager::new(PathBuf::from("/tmp/artifacts")),
            cache: crate::pipeline::CacheManager::new(PathBuf::from("/tmp/cache")),
            external_artifacts: None,
            bootstrap_mounts: Vec::new(),
        }
    }

    fn job(name: &str) -> JobSpec {
        JobSpec {
            name: name.into(),
            stage: "test".into(),
            commands: vec!["echo ok".into()],
            needs: Vec::new(),
            explicit_needs: false,
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
            rules: Vec::new(),
            only: Vec::new(),
            except: Vec::new(),
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
