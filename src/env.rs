use crate::gitlab::Job;
use crate::secrets::SecretsStore;
use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use std::collections::HashMap;
use std::env;
use std::path::Path;

pub fn collect_env_vars(patterns: &[String]) -> Result<Vec<(String, String)>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob =
            Glob::new(pattern).with_context(|| format!("invalid --env pattern '{pattern}'"))?;
        builder.add(glob);
    }
    let matcher = builder.build()?;

    let vars = env::vars()
        .filter(|(key, _)| matcher.is_match(key))
        .collect();
    Ok(vars)
}

pub fn build_job_env(
    base_env: &[(String, String)],
    default_vars: &HashMap<String, String>,
    job: &Job,
    secrets: &SecretsStore,
    workdir: &Path,
    run_id: &str,
) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let mut push = |key: &str, value: &str| {
        if let Some(existing) = env.iter_mut().find(|(k, _)| k == key) {
            existing.1 = value.to_string();
        } else {
            env.push((key.to_string(), value.to_string()));
        }
    };

    for (key, value) in base_env {
        push(key, value);
    }
    for (key, value) in default_vars {
        push(key, value);
    }
    for (key, value) in &job.variables {
        push(key, value);
    }

    push("CI", "true");
    push("GITLAB_CI", "true");
    push("CI_JOB_NAME", &job.name);
    push("CI_JOB_STAGE", &job.stage);
    push("CI_PROJECT_DIR", &workdir.display().to_string());
    push("CI_PIPELINE_ID", run_id);

    if secrets.has_secrets() {
        secrets.extend_env(&mut env);
    }

    env
}
