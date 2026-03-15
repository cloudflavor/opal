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
    container_workdir: &Path,
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
    push("CI_PIPELINE_ID", run_id);
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

fn expand_value(value: &str, lookup: &HashMap<String, String>) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitlab::Job;
    use crate::secrets::SecretsStore;
    use std::collections::HashMap;

    #[test]
    fn expands_env_references() {
        let job = Job {
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
            artifacts: Vec::new(),
            cache: Vec::new(),
            image: None,
            variables: HashMap::from([("CARGO_HOME".into(), "$CI_PROJECT_DIR/.cargo".into())]),
            services: Vec::new(),
            timeout: None,
            retry: Default::default(),
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
            "1",
            &HashMap::from([("CI_PROJECT_DIR".into(), "/workspace".into())]),
        );
        let map: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map.get("CI_PROJECT_DIR").unwrap(), "/workspace");
        assert_eq!(map.get("CARGO_HOME").unwrap(), "/workspace/.cargo");
    }
}
