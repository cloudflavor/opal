use super::ExecutorCore;
use crate::display::{self, indent_block};
use crate::env::expand_value;
use crate::execution_plan::ExecutableJob;
use crate::model::JobSpec;
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

    if !exec.config.enable_tui {
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
        display::print_line(format!("{} {}", image_label, job_image));

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

    exec.runtime_state
        .track_running_container(&planned.instance.job.name, &container_name);
    Ok(JobRunInfo { container_name })
}

pub(super) fn resolve_job_image(exec: &ExecutorCore, job: &JobSpec) -> Result<String> {
    resolve_job_image_with_env(exec, job, None)
}

pub(super) fn resolve_job_image_with_env(
    exec: &ExecutorCore,
    job: &JobSpec,
    env_lookup: Option<&HashMap<String, String>>,
) -> Result<String> {
    let template = if let Some(image) = job.image.as_ref() {
        image.clone()
    } else if let Some(image) = exec.pipeline.defaults.image.as_ref() {
        image.clone()
    } else if let Some(image) = exec.config.image.clone() {
        image
    } else {
        return Err(anyhow!(
            "job '{}' has no image (use --base-image or set image in pipeline/job)",
            job.name
        ));
    };

    if !template.contains('$') {
        return Ok(template);
    }

    if let Some(map) = env_lookup {
        Ok(expand_value(&template, map))
    } else {
        let owned_lookup: HashMap<String, String> = exec.job_env(job).into_iter().collect();
        Ok(expand_value(&template, &owned_lookup))
    }
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

        assert_eq!(image, "registry.example.com/linux:latest");
    }

    fn test_core() -> ExecutorCore {
        ExecutorCore {
            config: ExecutorConfig {
                pipeline: PathBuf::from("pipelines/tests/filters.gitlab-ci.yml"),
                workdir: PathBuf::from("."),
                image: None,
                env_includes: Vec::new(),
                max_parallel_jobs: 1,
                enable_tui: false,
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
