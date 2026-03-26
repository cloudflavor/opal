use crate::git;
use crate::model::{EnvironmentSpec, JobSpec};
use crate::naming::job_name_slug;
use crate::secrets::SecretsStore;
use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use std::collections::HashMap;
use std::env;
use std::path::Path;

pub fn build_include_lookup(
    workdir: &Path,
    host_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut lookup = host_env.clone();
    for (key, value) in inferred_ci_env(workdir, host_env) {
        lookup.entry(key).or_insert(value);
    }
    lookup
}

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
    push("CI_JOB_NAME_SLUG", &job_name_slug(&job.name));
    push("CI_JOB_STAGE", &job.stage);
    push("CI_PROJECT_DIR", &container_workdir.display().to_string());
    push("CI_BUILDS_DIR", &container_root.display().to_string());
    push("CI_PIPELINE_ID", run_id);
    push("OPAL_IN_OPAL", "1");

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

pub fn expand_environment(
    environment: &EnvironmentSpec,
    lookup: &HashMap<String, String>,
) -> EnvironmentSpec {
    EnvironmentSpec {
        name: expand_value(&environment.name, lookup),
        url: environment
            .url
            .as_ref()
            .map(|value| expand_value(value, lookup)),
        on_stop: environment
            .on_stop
            .as_ref()
            .map(|value| expand_value(value, lookup)),
        auto_stop_in: environment.auto_stop_in,
        action: environment.action,
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
                        let expr: String = chars[idx + 2..end].iter().collect();
                        if let Some((name, default)) = expr.split_once(":-") {
                            if let Some(val) = lookup.get(name).filter(|val| !val.is_empty()) {
                                output.push_str(val);
                            } else {
                                output.push_str(&expand_value(default, lookup));
                            }
                        } else if let Some(val) = lookup.get(&expr) {
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
    if let Some(branch) = host_env
        .get("CI_COMMIT_BRANCH")
        .filter(|value| !value.is_empty())
    {
        inferred.push(("CI_COMMIT_BRANCH".into(), branch.clone()));
    } else if host_env
        .get("CI_COMMIT_TAG")
        .filter(|value| !value.is_empty())
        .or_else(|| {
            host_env
                .get("GIT_COMMIT_TAG")
                .filter(|value| !value.is_empty())
        })
        .is_none()
        && let Ok(branch) = git::current_branch(workdir)
        && !branch.is_empty()
    {
        inferred.push(("CI_COMMIT_BRANCH".into(), branch));
    }
    if let Some(tag) = host_env
        .get("CI_COMMIT_TAG")
        .filter(|value| !value.is_empty())
        .or_else(|| {
            host_env
                .get("GIT_COMMIT_TAG")
                .filter(|value| !value.is_empty())
        })
    {
        inferred.push(("CI_COMMIT_TAG".into(), tag.clone()));
    } else if let Ok(tag) = git::current_tag(workdir)
        && !tag.is_empty()
    {
        inferred.push(("CI_COMMIT_TAG".into(), tag));
    }
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
    if let Some(value) = host_env.get(key).filter(|value| !value.is_empty()) {
        env.push((key.to_string(), value.clone()));
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
    use crate::model::{
        ArtifactSpec, EnvironmentActionSpec, EnvironmentSpec, JobSpec, RetryPolicySpec,
    };
    use crate::secrets::SecretsStore;
    use anyhow::Result;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

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
            when: None,
            rules: Vec::new(),
            only: Vec::new(),
            except: Vec::new(),
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
        assert_eq!(
            map.get("CI_JOB_NAME_SLUG").map(String::as_str),
            Some("build")
        );
        assert_eq!(
            map.get("CI_PROJECT_DIR").map(String::as_str),
            Some("/workspace")
        );
        assert_eq!(
            map.get("CARGO_HOME").map(String::as_str),
            Some("/workspace/.cargo")
        );
    }

    #[test]
    fn expands_shell_style_default_fallbacks() {
        let lookup = HashMap::from([
            ("CI_COMMIT_REF_SLUG".into(), "main".into()),
            ("CI_ENVIRONMENT_SLUG".into(), "review-main".into()),
        ]);

        assert_eq!(
            expand_value("review/${CI_COMMIT_REF_SLUG:-local}", &lookup),
            "review/main"
        );
        assert_eq!(
            expand_value(
                "https://${CI_ENVIRONMENT_SLUG:-fallback}.example.com",
                &lookup
            ),
            "https://review-main.example.com"
        );
        assert_eq!(
            expand_value("review/${MISSING_VAR:-local}", &lookup),
            "review/local"
        );
    }

    #[test]
    fn expands_environment_metadata() {
        let environment = EnvironmentSpec {
            name: "review/${CI_COMMIT_REF_SLUG:-local}".into(),
            url: Some("https://${CI_ENVIRONMENT_SLUG:-fallback}.example.com".into()),
            on_stop: Some("stop-${CI_COMMIT_REF_SLUG:-local}".into()),
            auto_stop_in: None,
            action: EnvironmentActionSpec::Start,
        };
        let expanded = expand_environment(
            &environment,
            &HashMap::from([
                ("CI_COMMIT_REF_SLUG".into(), "main".into()),
                ("CI_ENVIRONMENT_SLUG".into(), "review-main".into()),
            ]),
        );

        assert_eq!(expanded.name, "review/main");
        assert_eq!(
            expanded.url.as_deref(),
            Some("https://review-main.example.com")
        );
        assert_eq!(expanded.on_stop.as_deref(), Some("stop-main"));
    }

    #[test]
    fn infers_tagged_ref_vars_for_job_environment() -> Result<()> {
        let dir = init_repo_with_commit_and_tag("v1.2.3")?;

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
        Ok(())
    }

    #[test]
    fn tagged_job_environment_does_not_infer_branch() -> Result<()> {
        let dir = init_repo_with_commit_and_tag("v1.2.3")?;

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
            &HashMap::from([
                ("CI_COMMIT_TAG".into(), "v1.2.3".into()),
                ("CI_COMMIT_REF_NAME".into(), "v1.2.3".into()),
            ]),
        );
        let map: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map.get("CI_COMMIT_TAG").map(String::as_str), Some("v1.2.3"));
        assert!(!map.contains_key("CI_COMMIT_BRANCH"));
        Ok(())
    }

    #[test]
    fn secret_file_env_uses_absolute_container_path() -> Result<()> {
        let temp_root = temp_path("env-secret-file");
        let secrets_root = temp_root.join(".opal").join("env");
        fs::create_dir_all(&secrets_root)?;
        fs::write(secrets_root.join("API_TOKEN"), "super-secret")?;
        let secrets = SecretsStore::load(&temp_root)?;
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
        };

        let env = build_job_env(
            &[],
            &HashMap::new(),
            &job,
            &secrets,
            &temp_root,
            Path::new("/builds/workspace"),
            Path::new("/builds"),
            "1",
            &HashMap::new(),
        );
        let map: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(
            map.get("API_TOKEN_FILE").map(String::as_str),
            Some("/opal/secrets/API_TOKEN")
        );

        let _ = fs::remove_dir_all(temp_root);
        Ok(())
    }

    #[test]
    fn maps_git_commit_tag_into_ci_tag_variables() {
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
            &HashMap::from([("GIT_COMMIT_TAG".into(), "v9.9.9".into())]),
        );
        let map: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map.get("CI_COMMIT_TAG").map(String::as_str), Some("v9.9.9"));
        assert_eq!(
            map.get("CI_COMMIT_REF_NAME").map(String::as_str),
            Some("v9.9.9")
        );
        assert_eq!(
            map.get("CI_COMMIT_REF_SLUG").map(String::as_str),
            Some("v999")
        );
        assert!(!map.contains_key("CI_COMMIT_BRANCH"));
    }

    #[test]
    fn build_job_env_includes_legacy_dotopal_secrets() -> Result<()> {
        let temp_root = temp_path("env-legacy-secret-file");
        let dotopal = temp_root.join(".opal");
        fs::create_dir_all(&dotopal)?;
        fs::write(dotopal.join("QUAY_USERNAME"), "robot-user")?;
        let secrets = SecretsStore::load(&temp_root)?;
        let job = JobSpec {
            name: "container-release".into(),
            stage: "publish".into(),
            commands: Vec::new(),
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
        };

        let env = build_job_env(
            &[],
            &HashMap::new(),
            &job,
            &secrets,
            &temp_root,
            Path::new("/builds/workspace"),
            Path::new("/builds"),
            "1",
            &HashMap::new(),
        );
        let map: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(
            map.get("QUAY_USERNAME").map(String::as_str),
            Some("robot-user")
        );

        let _ = fs::remove_dir_all(temp_root);
        Ok(())
    }

    fn temp_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("opal-{prefix}-{nanos}"))
    }
}
