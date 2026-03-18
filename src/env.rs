use crate::git;
use crate::model::JobSpec;
use crate::naming::job_name_slug;
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

#[allow(clippy::too_many_arguments)]
pub fn build_job_env(
    base_env: &[(String, String)],
    default_vars: &HashMap<String, String>,
    job: &JobSpec,
    secrets: &SecretsStore,
    host_workdir: &Path,
    container_workdir: &Path,
    container_root: &Path,
    run_id: &str,
    host_env: &HashMap<String, String>,
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
    push("CI_PROJECT_DIR", &container_workdir.display().to_string());
    push("CI_BUILDS_DIR", &container_root.display().to_string());
    push("CI_PIPELINE_ID", run_id);

    for (key, value) in inferred_ci_env(host_workdir, host_env) {
        push(&key, &value);
    }

    if let Some(timeout) = job.timeout {
        push("CI_JOB_TIMEOUT", &timeout.as_secs().to_string());
    }

    if secrets.has_secrets() {
        secrets.extend_env(&mut env);
    }

    expand_env_list(&mut env[..], host_env);

    env
}

pub fn expand_env_list(env: &mut [(String, String)], host_env: &HashMap<String, String>) {
    let mut lookup: HashMap<String, String> = HashMap::with_capacity(host_env.len() + env.len());
    for (key, value) in host_env {
        lookup.insert(key.clone(), value.clone());
    }
    for (key, value) in env.iter() {
        lookup.entry(key.clone()).or_insert_with(|| value.clone());
    }
    for (key, value) in env.iter_mut() {
        let expanded = expand_value(value, &lookup);
        *value = expanded.clone();
        lookup.insert(key.clone(), expanded);
    }
}

pub fn expand_value(value: &str, lookup: &HashMap<String, String>) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut idx = 0;
    let mut output = String::new();
    while idx < chars.len() {
        let ch = chars[idx];
        if ch == '$' && idx + 1 < chars.len() {
            match chars[idx + 1] {
                '$' => {
                    output.push('$');
                    idx += 2;
                    continue;
                }
                '{' => {
                    let mut end = idx + 2;
                    while end < chars.len() && chars[end] != '}' {
                        end += 1;
                    }
                    if end < chars.len() {
                        let name: String = chars[idx + 2..end].iter().collect();
                        if let Some(val) = lookup.get(&name) {
                            output.push_str(val);
                        }
                        idx = end + 1;
                        continue;
                    }
                }
                c if is_var_char(c) => {
                    let mut end = idx + 1;
                    while end < chars.len() && is_var_char(chars[end]) {
                        end += 1;
                    }
                    let name: String = chars[idx + 1..end].iter().collect();
                    if let Some(val) = lookup.get(&name) {
                        output.push_str(val);
                    }
                    idx = end;
                    continue;
                }
                _ => {}
            }
        }
        output.push(ch);
        idx += 1;
    }
    output
}

fn is_var_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn inferred_ci_env(workdir: &Path, host_env: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut inferred = Vec::new();

    insert_inferred_env(
        &mut inferred,
        "CI_PIPELINE_SOURCE",
        host_env,
        Some(|| Ok("push".to_string())),
    );
    insert_inferred_env(
        &mut inferred,
        "CI_COMMIT_BRANCH",
        host_env,
        Some(|| git::current_branch(workdir)),
    );
    insert_inferred_env(
        &mut inferred,
        "CI_COMMIT_TAG",
        host_env,
        Some(|| git::current_tag(workdir)),
    );
    insert_inferred_env(
        &mut inferred,
        "CI_DEFAULT_BRANCH",
        host_env,
        Some(|| git::default_branch(workdir)),
    );

    if host_env
        .get("CI_COMMIT_REF_NAME")
        .is_none_or(|value| value.is_empty())
    {
        if let Some(tag) = host_env
            .get("CI_COMMIT_TAG")
            .filter(|value| !value.is_empty())
            .cloned()
            .or_else(|| {
                inferred
                    .iter()
                    .find(|(key, _)| key == "CI_COMMIT_TAG")
                    .map(|(_, value)| value.clone())
            })
        {
            inferred.push(("CI_COMMIT_REF_NAME".into(), tag));
        } else if let Some(branch) = host_env
            .get("CI_COMMIT_BRANCH")
            .filter(|value| !value.is_empty())
            .cloned()
            .or_else(|| {
                inferred
                    .iter()
                    .find(|(key, _)| key == "CI_COMMIT_BRANCH")
                    .map(|(_, value)| value.clone())
            })
        {
            inferred.push(("CI_COMMIT_REF_NAME".into(), branch));
        }
    }
    if host_env
        .get("CI_COMMIT_REF_SLUG")
        .is_none_or(|value| value.is_empty())
        && let Some(ref_name) = host_env
            .get("CI_COMMIT_REF_NAME")
            .filter(|value| !value.is_empty())
            .cloned()
            .or_else(|| {
                inferred
                    .iter()
                    .find(|(key, _)| key == "CI_COMMIT_REF_NAME")
                    .map(|(_, value)| value.clone())
            })
    {
        let slug = job_name_slug(&ref_name);
        if !slug.is_empty() {
            inferred.push(("CI_COMMIT_REF_SLUG".into(), slug));
        }
    }

    inferred
}

fn insert_inferred_env<F>(
    env: &mut Vec<(String, String)>,
    key: &str,
    host_env: &HashMap<String, String>,
    fallback: Option<F>,
) where
    F: FnOnce() -> Result<String>,
{
    if host_env.get(key).is_some_and(|value| !value.is_empty()) {
        return;
    }
    if let Some(fallback) = fallback
        && let Ok(value) = fallback()
        && !value.is_empty()
    {
        env.push((key.to_string(), value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::init_repo_with_commit_and_tag;
    use crate::model::{ArtifactSpec, JobSpec, RetryPolicySpec};
    use crate::secrets::SecretsStore;
    use std::collections::HashMap;

    #[test]
    fn expands_env_references() {
        let job = JobSpec {
            name: "lint".into(),
            stage: "test".into(),
            commands: Vec::new(),
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
            variables: HashMap::from([("CARGO_HOME".into(), "$CI_PROJECT_DIR/.cargo".into())]),
            services: Vec::new(),
            timeout: None,
            retry: RetryPolicySpec::default(),
            interruptible: false,
            resource_group: None,
            parallel: None,
            tags: Vec::new(),
            environment: None,
        };
        let env = build_job_env(
            &[],
            &HashMap::new(),
            &job,
            &SecretsStore::default(),
            Path::new("/workspace"),
            Path::new("/workspace"),
            Path::new("/builds"),
            "1",
            &HashMap::from([("CI_PROJECT_DIR".into(), "/workspace".into())]),
        );
        let map: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map.get("CI_PROJECT_DIR").unwrap(), "/workspace");
        assert_eq!(map.get("CARGO_HOME").unwrap(), "/workspace/.cargo");
    }

    #[test]
    fn infers_tagged_ref_vars_for_job_environment() {
        let dir = init_repo_with_commit_and_tag("v1.2.3");

        let job = JobSpec {
            name: "release-artifacts".into(),
            stage: "release".into(),
            commands: Vec::new(),
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
        };

        let env = build_job_env(
            &[],
            &HashMap::new(),
            &job,
            &SecretsStore::default(),
            dir.path(),
            Path::new("/workspace"),
            Path::new("/builds"),
            "1",
            &HashMap::new(),
        );
        let map: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map.get("CI_COMMIT_TAG").map(String::as_str), Some("v1.2.3"));
        assert_eq!(
            map.get("CI_COMMIT_REF_NAME").map(String::as_str),
            Some("v1.2.3")
        );
    }
}
