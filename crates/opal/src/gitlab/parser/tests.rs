use super::super::{ArtifactWhen, CacheKey, Job, PipelineGraph};
use anyhow::{Result, anyhow};
use petgraph::graph::NodeIndex;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn parses_stage_and_job_order() -> Result<()> {
    let yaml = r#"
stages:
  - build
  - test

build-job:
  stage: build
  script:
    - echo build

second-build:
  stage: build
  script: echo build 2

test-job:
  stage: test
  script:
    - echo test
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml)?;
    assert_eq!(pipeline.stages.len(), 2);
    assert_eq!(pipeline.stages[0].name, "build");
    assert_eq!(pipeline.stages[1].name, "test");

    let build_jobs: Vec<&Job> = pipeline.stages[0]
        .jobs
        .iter()
        .map(|idx| &pipeline.graph[*idx])
        .collect();
    assert_eq!(build_jobs.len(), 2);
    assert_eq!(build_jobs[0].name, "build-job");
    assert_eq!(build_jobs[1].name, "second-build");

    let test_jobs: Vec<&Job> = pipeline.stages[1]
        .jobs
        .iter()
        .map(|idx| &pipeline.graph[*idx])
        .collect();
    assert_eq!(test_jobs.len(), 1);
    assert_eq!(test_jobs[0].name, "test-job");
    Ok(())
}

#[test]
fn resolves_reference_tags() -> Result<()> {
    let yaml = r#"
stages: [build]

.shared:
  script:
    - echo shared
  variables:
    SHARED_VAR: shared-value

build-job:
  stage: build
  script: !reference [.shared, script]
  variables:
    COPIED: !reference [.shared, variables, SHARED_VAR]
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml)?;
    assert_eq!(pipeline.stages.len(), 1);
    let build_stage = &pipeline.stages[0];
    assert_eq!(build_stage.jobs.len(), 1);
    let job = &pipeline.graph[build_stage.jobs[0]];
    assert_eq!(job.commands, vec!["echo shared"]);
    assert_eq!(
        job.variables.get("COPIED").map(|value| value.as_str()),
        Some("shared-value")
    );
    Ok(())
}

#[test]
fn includes_local_fragment() -> Result<()> {
    let dir = tempdir()?;
    let fragment_path = dir.path().join("fragment.yml");
    fs::write(
        &fragment_path,
        r#"
fragment-job:
  stage: build
  script:
    - echo fragment
"#,
    )?;

    let main_path = dir.path().join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build
include:
  - local: fragment.yml

main-job:
  stage: build
  script:
    - echo main
"#,
    )?;

    let pipeline = PipelineGraph::from_path(&main_path)?;
    assert_eq!(pipeline.stages.len(), 1);
    assert_eq!(pipeline.stages[0].jobs.len(), 2);
    assert!(
        pipeline
            .graph
            .node_weights()
            .any(|job| job.name == "fragment-job")
    );
    assert!(
        pipeline
            .graph
            .node_weights()
            .any(|job| job.name == "main-job")
    );
    Ok(())
}

#[test]
fn includes_local_paths_from_repo_root() -> Result<()> {
    let dir = tempdir()?;
    git2::Repository::init(dir.path())?;

    let fragment_path = dir.path().join("fragment.yml");
    fs::write(
        &fragment_path,
        r#"
fragment-job:
  stage: build
  script:
    - echo fragment
"#,
    )?;

    let ci_dir = dir.path().join("ci");
    fs::create_dir_all(&ci_dir)?;
    let main_path = ci_dir.join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build
include:
  - local: fragment.yml

main-job:
  stage: build
  script:
    - echo main
"#,
    )?;

    let pipeline = PipelineGraph::from_path(&main_path)?;
    assert!(
        pipeline
            .graph
            .node_weights()
            .any(|job| job.name == "fragment-job")
    );
    assert!(
        pipeline
            .graph
            .node_weights()
            .any(|job| job.name == "main-job")
    );
    Ok(())
}

#[test]
fn includes_local_glob_from_repo_root() -> Result<()> {
    let dir = tempdir()?;
    git2::Repository::init(dir.path())?;

    let configs_dir = dir.path().join("configs");
    fs::create_dir_all(&configs_dir)?;
    fs::write(
        configs_dir.join("a.yml"),
        r#"
job-a:
  stage: build
  script:
    - echo a
"#,
    )?;
    fs::write(
        configs_dir.join("b.yml"),
        r#"
job-b:
  stage: build
  script:
    - echo b
"#,
    )?;

    let ci_dir = dir.path().join("ci");
    fs::create_dir_all(&ci_dir)?;
    let main_path = ci_dir.join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build
include:
  - local: configs/*.yml

main-job:
  stage: build
  script:
    - echo main
"#,
    )?;

    let pipeline = PipelineGraph::from_path(&main_path)?;
    assert!(pipeline.graph.node_weights().any(|job| job.name == "job-a"));
    assert!(pipeline.graph.node_weights().any(|job| job.name == "job-b"));
    assert!(
        pipeline
            .graph
            .node_weights()
            .any(|job| job.name == "main-job")
    );
    Ok(())
}

#[test]
fn includes_local_paths_expand_environment_variables() -> Result<()> {
    let dir = tempdir()?;
    git2::Repository::init(dir.path())?;

    fs::write(
        dir.path().join("fragment.yml"),
        r#"
fragment-job:
  stage: build
  script:
    - echo fragment
"#,
    )?;

    let ci_dir = dir.path().join("ci");
    fs::create_dir_all(&ci_dir)?;
    let main_path = ci_dir.join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build
include:
  - local: $INCLUDE_FILE

main-job:
  stage: build
  script:
    - echo main
"#,
    )?;

    let pipeline = PipelineGraph::from_path_with_env(
        &main_path,
        HashMap::from([("INCLUDE_FILE".to_string(), "fragment.yml".to_string())]),
        None,
    )?;

    assert!(
        pipeline
            .graph
            .node_weights()
            .any(|job| job.name == "fragment-job")
    );
    assert!(
        pipeline
            .graph
            .node_weights()
            .any(|job| job.name == "main-job")
    );
    Ok(())
}

#[test]
fn includes_local_files_list_from_repo_root() -> Result<()> {
    let dir = tempdir()?;
    git2::Repository::init(dir.path())?;

    let parts_dir = dir.path().join("parts");
    fs::create_dir_all(&parts_dir)?;
    fs::write(
        parts_dir.join("a.yml"),
        r#"
job-a:
  stage: build
  script:
    - echo a
"#,
    )?;
    fs::write(
        parts_dir.join("b.yml"),
        r#"
job-b:
  stage: build
  script:
    - echo b
"#,
    )?;

    let main_path = dir.path().join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build
include:
  files:
    - /parts/a.yml
    - /parts/b.yml

main-job:
  stage: build
  script:
    - echo main
"#,
    )?;

    let pipeline = PipelineGraph::from_path(&main_path)?;
    assert!(pipeline.graph.node_weights().any(|job| job.name == "job-a"));
    assert!(pipeline.graph.node_weights().any(|job| job.name == "job-b"));
    assert!(
        pipeline
            .graph
            .node_weights()
            .any(|job| job.name == "main-job")
    );
    Ok(())
}

#[test]
fn includes_local_non_yaml_file_errors() {
    let dir = tempdir().expect("tempdir");
    git2::Repository::init(dir.path()).expect("init repo");

    fs::write(dir.path().join("fragment.txt"), "not yaml").expect("write fragment");

    let main_path = dir.path().join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build
include:
  - local: /fragment.txt

main-job:
  stage: build
  script:
    - echo main
"#,
    )
    .expect("write main");

    let err = PipelineGraph::from_path(&main_path).expect_err("non-yaml include must error");
    assert!(
        err.to_string()
            .contains("must reference a .yml or .yaml file")
    );
}

#[test]
fn includes_file_alias_as_local_path() -> Result<()> {
    let dir = tempdir()?;
    git2::Repository::init(dir.path())?;

    fs::write(
        dir.path().join("fragment.yml"),
        r#"
fragment-job:
  stage: build
  script:
    - echo fragment
"#,
    )?;

    let main_path = dir.path().join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build
include:
  - file: /fragment.yml

main-job:
  stage: build
  script:
    - echo main
"#,
    )?;

    let pipeline = PipelineGraph::from_path(&main_path)?;
    assert!(
        pipeline
            .graph
            .node_weights()
            .any(|job| job.name == "fragment-job")
    );
    assert!(
        pipeline
            .graph
            .node_weights()
            .any(|job| job.name == "main-job")
    );
    Ok(())
}

#[test]
fn retry_max_above_two_errors() {
    let dir = tempdir().expect("tempdir");
    let main_path = dir.path().join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build

build:
  stage: build
  retry: 3
  script:
    - echo hi
"#,
    )
    .expect("write main");

    let err = PipelineGraph::from_path(&main_path).expect_err("retry max must error");
    assert!(
        err.to_string()
            .contains("job 'build'.retry must be 0, 1, or 2")
    );
}

#[test]
fn retry_exit_codes_parses_single_value() {
    let dir = tempdir().expect("tempdir");
    let main_path = dir.path().join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build

build:
  stage: build
  retry:
    max: 1
    exit_codes: 137
  script:
    - echo hi
"#,
    )
    .expect("write main");

    let pipeline = PipelineGraph::from_path(&main_path).expect("retry exit_codes must parse");
    let job = pipeline
        .graph
        .node_weights()
        .find(|job| job.name == "build")
        .expect("build job present");
    assert_eq!(job.retry.max, 1);
    assert_eq!(job.retry.exit_codes, vec![137]);
}

#[test]
fn retry_exit_codes_rejects_negative_values() {
    let dir = tempdir().expect("tempdir");
    let main_path = dir.path().join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build

build:
  stage: build
  retry:
    max: 1
    exit_codes:
      - -1
  script:
    - echo hi
"#,
    )
    .expect("write main");

    let err = PipelineGraph::from_path(&main_path).expect_err("negative exit code must error");
    assert!(
        err.to_string()
            .contains("job 'build'.retry.exit_codes must contain non-negative integers")
    );
}

#[test]
fn parses_artifacts_reports_dotenv() {
    let dir = tempdir().expect("tempdir");
    let main_path = dir.path().join("main.yml");
    fs::write(
        &main_path,
        r#"
stages:
  - build

build:
  stage: build
  script:
    - echo hi
  artifacts:
    reports:
      dotenv: tests-temp/dotenv/build.env
"#,
    )
    .expect("write main");

    let pipeline = PipelineGraph::from_path(&main_path).expect("pipeline parses");
    let job = pipeline
        .graph
        .node_weights()
        .find(|job| job.name == "build")
        .expect("build job present");

    assert_eq!(
        job.artifacts.report_dotenv.as_deref(),
        Some(std::path::Path::new("tests-temp/dotenv/build.env"))
    );
}

#[test]
fn yaml_merge_keys_work_in_variables_mapping() {
    let pipeline = PipelineGraph::from_yaml_str(
        r#"
stages:
  - test

.base-vars: &base_vars
  SHARED_FLAG: from-anchor
  OVERRIDE_ME: old

merged-vars:
  stage: test
  variables:
    <<: *base_vars
    OVERRIDE_ME: new
  script:
    - echo ok
"#,
    )
    .expect("pipeline parses");

    let job = pipeline
        .graph
        .node_weights()
        .find(|job| job.name == "merged-vars")
        .expect("job present");

    assert_eq!(
        job.variables.get("SHARED_FLAG").map(String::as_str),
        Some("from-anchor")
    );
    assert_eq!(
        job.variables.get("OVERRIDE_ME").map(String::as_str),
        Some("new")
    );
}

#[test]
fn yaml_merge_keys_work_in_job_mapping() {
    let pipeline = PipelineGraph::from_yaml_str(
        r#"
stages:
  - test

.job-template: &job_template
  stage: test
  script:
    - echo ok
  variables:
    INNER_FLAG: inner

merged-job:
  <<: *job_template
  variables:
    <<: { INNER_FLAG: inner, EXTRA_FLAG: extra }
"#,
    )
    .expect("pipeline parses");

    let job = pipeline
        .graph
        .node_weights()
        .find(|job| job.name == "merged-job")
        .expect("job present");

    assert_eq!(job.stage, "test");
    assert_eq!(job.commands, vec!["echo ok".to_string()]);
    assert_eq!(
        job.variables.get("INNER_FLAG").map(String::as_str),
        Some("inner")
    );
    assert_eq!(
        job.variables.get("EXTRA_FLAG").map(String::as_str),
        Some("extra")
    );
}

#[test]
fn parses_image_docker_platform() {
    let pipeline = PipelineGraph::from_yaml_str(
        r#"
stages:
  - test

platform-job:
  stage: test
  image:
    name: docker.io/library/alpine:3.19
    docker:
      platform: linux/arm64/v8
  script:
    - echo ok
"#,
    )
    .expect("pipeline parses");

    let job = pipeline
        .graph
        .node_weights()
        .find(|job| job.name == "platform-job")
        .expect("job present");

    let image = job.image.as_ref().expect("image present");
    assert_eq!(image.name, "docker.io/library/alpine:3.19");
    assert_eq!(image.docker_platform.as_deref(), Some("linux/arm64/v8"));
}

#[test]
fn parses_image_entrypoint_and_docker_user() {
    let pipeline = PipelineGraph::from_yaml_str(
        r#"
stages:
  - test

platform-job:
  stage: test
  image:
    name: docker.io/library/alpine:3.19
    entrypoint: [""]
    docker:
      user: 1000:1000
  script:
    - echo ok
"#,
    )
    .expect("pipeline parses");

    let job = pipeline
        .graph
        .node_weights()
        .find(|job| job.name == "platform-job")
        .expect("job present");

    let image = job.image.as_ref().expect("image present");
    assert_eq!(image.entrypoint, vec![""]);
    assert_eq!(image.docker_user.as_deref(), Some("1000:1000"));
}

#[test]
fn parses_service_docker_platform_and_user() {
    let pipeline = PipelineGraph::from_yaml_str(
        r#"
stages:
  - test

service-job:
  stage: test
  image: docker.io/library/alpine:3.19
  services:
    - name: docker.io/library/redis:7.2
      alias: cache
      docker:
        platform: linux/arm64/v8
        user: 1000:1000
  script:
    - echo ok
"#,
    )
    .expect("pipeline parses");

    let job = pipeline
        .graph
        .node_weights()
        .find(|job| job.name == "service-job")
        .expect("job present");

    let service = job.services.first().expect("service present");
    assert_eq!(service.image, "docker.io/library/redis:7.2");
    assert_eq!(service.aliases, vec!["cache"]);
    assert_eq!(service.docker_platform.as_deref(), Some("linux/arm64/v8"));
    assert_eq!(service.docker_user.as_deref(), Some("1000:1000"));
}

#[test]
fn inherit_default_controls_modeled_default_keywords() {
    let pipeline = PipelineGraph::from_yaml_str(
        r#"
stages:
  - test

default:
  image: docker.io/library/alpine:3.19
  before_script:
    - echo before
  after_script:
    - echo after
  cache:
    key: default-cache
    paths:
      - tests-temp/default-cache/
  services:
    - docker.io/library/redis:7.2
  timeout: 10m
  retry: 2
  interruptible: true

inherit-none:
  stage: test
  inherit:
    default: false
  image: docker.io/library/alpine:3.19
  script:
    - echo none

inherit-some:
  stage: test
  inherit:
    default: [image, retry, interruptible]
  script:
    - echo some
"#,
    )
    .expect("pipeline parses");

    let none = pipeline
        .graph
        .node_weights()
        .find(|job| job.name == "inherit-none")
        .expect("job present");
    assert!(!none.inherit_default_image);
    assert!(!none.inherit_default_before_script);
    assert!(!none.inherit_default_after_script);
    assert!(!none.inherit_default_cache);
    assert!(!none.inherit_default_services);
    assert!(!none.inherit_default_timeout);
    assert!(!none.inherit_default_retry);
    assert!(!none.inherit_default_interruptible);
    assert!(none.cache.is_empty());
    assert!(none.services.is_empty());
    assert_eq!(none.timeout, None);
    assert_eq!(none.retry.max, 0);
    assert!(!none.interruptible);

    let some = pipeline
        .graph
        .node_weights()
        .find(|job| job.name == "inherit-some")
        .expect("job present");
    assert!(some.inherit_default_image);
    assert!(!some.inherit_default_before_script);
    assert!(!some.inherit_default_after_script);
    assert!(!some.inherit_default_cache);
    assert!(!some.inherit_default_services);
    assert!(!some.inherit_default_timeout);
    assert!(some.inherit_default_retry);
    assert!(some.inherit_default_interruptible);
    assert_eq!(
        some.image.as_ref().map(|image| image.name.as_str()),
        Some("docker.io/library/alpine:3.19")
    );
    assert!(some.cache.is_empty());
    assert!(some.services.is_empty());
    assert_eq!(some.timeout, None);
    assert_eq!(some.retry.max, 2);
    assert!(some.interruptible);
}

#[test]
fn include_cycle_errors() -> Result<()> {
    let dir = tempdir()?;
    let a_path = dir.path().join("a.yml");
    let b_path = dir.path().join("b.yml");
    fs::write(
        &a_path,
        format!(
            "
include:
  - local: {}
job-a:
  stage: build
  script: echo a
",
            b_path
                .file_name()
                .ok_or_else(|| anyhow!("missing b.yml filename"))?
                .to_string_lossy()
        ),
    )?;
    fs::write(
        &b_path,
        format!(
            "
include:
  - local: {}
job-b:
  stage: build
  script: echo b
",
            a_path
                .file_name()
                .ok_or_else(|| anyhow!("missing a.yml filename"))?
                .to_string_lossy()
        ),
    )?;

    let err = PipelineGraph::from_path(&a_path).expect_err("cycle must error");
    assert!(err.to_string().contains("include cycle"));
    Ok(())
}

#[test]
fn records_needs_dependencies() {
    let yaml = r#"
stages:
  - build
  - deploy

build-job:
  stage: build
  script:
    - echo build

deploy-job:
  stage: deploy
  needs:
    - build-job
  script:
    - echo deploy
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let build_idx = find_job(&pipeline, "build-job");
    let deploy_idx = find_job(&pipeline, "deploy-job");

    assert_eq!(
        pipeline.graph[deploy_idx].needs[0].job,
        "build-job".to_string()
    );
    assert!(pipeline.graph[deploy_idx].needs[0].needs_artifacts);
    assert!(pipeline.graph.contains_edge(build_idx, deploy_idx));
}

#[test]
fn parses_needs_without_artifacts() {
    let yaml = r#"
stages:
  - build
  - test

build-job:
  stage: build
  script:
    - echo build

test-job:
  stage: test
  needs:
    - job: build-job
      artifacts: false
  script:
    - echo test
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let test_idx = find_job(&pipeline, "test-job");
    assert_eq!(pipeline.graph[test_idx].needs.len(), 1);
    let need = &pipeline.graph[test_idx].needs[0];
    assert_eq!(need.job, "build-job");
    assert!(!need.needs_artifacts);
    assert!(!need.optional);
}

#[test]
fn parses_optional_needs() {
    let yaml = r#"
stages:
  - build
  - test

build-job:
  stage: build
  script:
    - echo build

maybe-job:
  stage: build
  script:
    - echo maybe

test-job:
  stage: test
  needs:
    - build-job
    - job: maybe-job
      optional: true
  script:
    - echo test
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let test_idx = find_job(&pipeline, "test-job");
    assert_eq!(pipeline.graph[test_idx].needs.len(), 2);
    let need0 = &pipeline.graph[test_idx].needs[0];
    assert_eq!(need0.job, "build-job");
    assert!(!need0.optional);
    let need1 = &pipeline.graph[test_idx].needs[1];
    assert_eq!(need1.job, "maybe-job");
    assert!(need1.optional);
}

#[test]
fn parses_artifacts_paths() {
    let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  artifacts:
    paths:
      - vendor/
      - output/report.txt
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let build_idx = find_job(&pipeline, "build-job");
    let job = &pipeline.graph[build_idx];
    assert_eq!(job.artifacts.paths.len(), 2);
    assert_eq!(job.artifacts.paths[0], PathBuf::from("vendor"));
    assert_eq!(job.artifacts.paths[1], PathBuf::from("output/report.txt"));
    assert_eq!(job.artifacts.when, ArtifactWhen::OnSuccess);
}

#[test]
fn parses_artifacts_when() {
    let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  artifacts:
    when: always
    paths:
      - output/report.txt
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let build_idx = find_job(&pipeline, "build-job");
    let job = &pipeline.graph[build_idx];
    assert_eq!(job.artifacts.when, ArtifactWhen::Always);
}

#[test]
fn parses_artifacts_exclude() {
    let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  artifacts:
    paths:
      - tests-temp/output/
    exclude:
      - tests-temp/output/**/*.log
      - tests-temp/output/ignore.txt
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let build_idx = find_job(&pipeline, "build-job");
    let job = &pipeline.graph[build_idx];
    assert_eq!(
        job.artifacts.exclude,
        vec![
            "tests-temp/output/**/*.log".to_string(),
            "tests-temp/output/ignore.txt".to_string()
        ]
    );
}

#[test]
fn parses_cache_fallback_keys() {
    let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  cache:
    key: cache-$CI_COMMIT_REF_SLUG
    fallback_keys:
      - cache-$CI_DEFAULT_BRANCH
      - cache-default
    paths:
      - vendor/
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let build_idx = find_job(&pipeline, "build-job");
    let job = &pipeline.graph[build_idx];
    assert_eq!(
        job.cache[0].fallback_keys,
        vec![
            "cache-$CI_DEFAULT_BRANCH".to_string(),
            "cache-default".to_string()
        ]
    );
}

#[test]
fn parses_cache_key_files_mapping() {
    let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  cache:
    key:
      files:
        - Cargo.lock
      prefix: $CI_JOB_NAME
    paths:
      - vendor/
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let build_idx = find_job(&pipeline, "build-job");
    let job = &pipeline.graph[build_idx];
    assert_eq!(
        job.cache[0].key,
        CacheKey::Files {
            files: vec![PathBuf::from("Cargo.lock")],
            prefix: Some("$CI_JOB_NAME".to_string()),
        }
    );
}

#[test]
fn errors_when_cache_key_files_has_more_than_two_paths() {
    let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  cache:
    key:
      files:
        - Cargo.lock
        - package-lock.json
        - yarn.lock
    paths:
      - vendor/
"#;

    let err = PipelineGraph::from_yaml_str(yaml).expect_err("pipeline should fail");
    assert!(
        err.to_string()
            .contains("cache key map supports at most two files"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn parses_artifacts_untracked() {
    let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  artifacts:
    untracked: true
    exclude:
      - tests-temp/output/**/*.log
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let build_idx = find_job(&pipeline, "build-job");
    let job = &pipeline.graph[build_idx];
    assert!(job.artifacts.untracked);
    assert_eq!(
        job.artifacts.exclude,
        vec!["tests-temp/output/**/*.log".to_string()]
    );
}

#[test]
fn parses_pipeline_and_job_images() {
    let yaml = r#"
image: rust:latest
stages:
  - build
  - test

build-job:
  stage: build
  image: rust:slim
  script:
    - echo build

test-job:
  stage: test
  script:
    - echo test
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    assert_eq!(
        pipeline
            .defaults
            .image
            .as_ref()
            .map(|image| image.name.as_str()),
        Some("rust:latest")
    );
    let build_idx = find_job(&pipeline, "build-job");
    assert_eq!(
        pipeline.graph[build_idx]
            .image
            .as_ref()
            .map(|image| image.name.as_str()),
        Some("rust:slim")
    );
    let test_idx = find_job(&pipeline, "test-job");
    assert_eq!(
        pipeline.graph[test_idx]
            .image
            .as_ref()
            .map(|image| image.name.as_str()),
        Some("rust:latest")
    );
}

#[test]
fn ignores_default_section_as_job() {
    let yaml = r#"
stages:
  - build

default:
  image: alpine:latest
  before_script:
    - echo before
  after_script:
    - echo after
  variables:
    GLOBAL_DEFAULT: foo

build-job:
  stage: build
  script:
    - echo build
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    assert_eq!(pipeline.stages.len(), 1);
    assert_eq!(pipeline.stages[0].jobs.len(), 1);
    assert_eq!(
        pipeline.defaults.before_script,
        vec!["echo before".to_string()]
    );
    assert_eq!(
        pipeline.defaults.after_script,
        vec!["echo after".to_string()]
    );
    assert_eq!(
        pipeline
            .defaults
            .variables
            .get("GLOBAL_DEFAULT")
            .map(String::as_str),
        Some("foo")
    );
    let job_idx = pipeline.stages[0].jobs[0];
    assert_eq!(pipeline.graph[job_idx].name, "build-job");
}

#[test]
fn parses_global_hooks() {
    let yaml = r#"
stages:
  - build

default:
  before_script:
    - echo before-one
    - echo before-two
  after_script: echo after

build-job:
  stage: build
  script: echo body
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    assert_eq!(
        pipeline.defaults.before_script,
        vec!["echo before-one".to_string(), "echo before-two".to_string()]
    );
    assert_eq!(
        pipeline.defaults.after_script,
        vec!["echo after".to_string()]
    );
}

#[test]
fn parses_variable_scopes() {
    let yaml = r#"
variables:
  GLOBAL_VAR: foo

default:
  variables:
    DEFAULT_VAR: bar

stages:
  - build

build-job:
  stage: build
  variables:
    JOB_VAR: baz
  script:
    - echo job
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    assert_eq!(
        pipeline
            .defaults
            .variables
            .get("GLOBAL_VAR")
            .map(String::as_str),
        Some("foo")
    );
    assert_eq!(
        pipeline
            .defaults
            .variables
            .get("DEFAULT_VAR")
            .map(String::as_str),
        Some("bar")
    );
    let job_idx = find_job(&pipeline, "build-job");
    assert_eq!(
        pipeline.graph[job_idx]
            .variables
            .get("JOB_VAR")
            .map(String::as_str),
        Some("baz")
    );
}

#[test]
fn ignores_non_job_sections_without_scripts() {
    let yaml = r#"
stages:
  - build

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "push"

build-job:
  stage: build
  script:
    - echo build
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    assert_eq!(pipeline.stages.len(), 1);
    assert_eq!(pipeline.stages[0].jobs.len(), 1);
    let job_idx = pipeline.stages[0].jobs[0];
    assert_eq!(pipeline.graph[job_idx].name, "build-job");
}

#[test]
fn errors_when_job_missing_script() {
    let yaml = r#"
stages:
  - build

broken-job:
  stage: build
"#;

    let err = PipelineGraph::from_yaml_str(yaml).expect_err("missing script should error");
    assert!(err.to_string().contains("must define a script"));
}

#[test]
fn ignores_hidden_jobs_starting_with_dot() {
    let yaml = r#"
stages:
  - build

.template:
  script:
    - echo template

build-job:
  stage: build
  script:
    - echo build
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    assert_eq!(pipeline.stages[0].jobs.len(), 1);
    let job_idx = pipeline.stages[0].jobs[0];
    assert_eq!(pipeline.graph[job_idx].name, "build-job");
}

#[test]
fn job_can_extend_hidden_template() {
    let yaml = r#"
stages:
  - build

.base-template:
  stage: build
  script:
    - echo from template
  artifacts:
    paths:
      - template.txt

child-job:
  extends: .base-template
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let job_idx = find_job(&pipeline, "child-job");
    let job = &pipeline.graph[job_idx];
    assert_eq!(job.stage, "build");
    assert_eq!(job.commands, vec!["echo from template"]);
    assert_eq!(job.artifacts.paths, vec![PathBuf::from("template.txt")]);
}

#[test]
fn job_merges_multiple_extends_in_order() {
    let yaml = r#"
stages:
  - test

.lint-template:
  script:
    - echo lint
  artifacts:
    paths:
      - lint.txt

.test-template:
  stage: test
  script:
    - echo tests
  artifacts:
    paths:
      - tests.txt

combined:
  extends:
    - .lint-template
    - .test-template
"#;

    let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
    let job_idx = find_job(&pipeline, "combined");
    let job = &pipeline.graph[job_idx];
    assert_eq!(job.commands, vec!["echo tests"]);
    assert_eq!(job.artifacts.paths, vec![PathBuf::from("tests.txt")]);
    assert_eq!(job.stage, "test");
}

#[test]
fn errors_on_extends_cycle() {
    let yaml = r#"
stages:
  - build

.a:
  extends: .b
  script:
    - echo a

.b:
  extends: .a
  script:
    - echo b

job:
  extends: .a
"#;

    let err = PipelineGraph::from_yaml_str(yaml).expect_err("cycle must error");
    assert!(err.to_string().contains("cyclical extends"));
}

#[test]
fn errors_on_unknown_extended_template() {
    let yaml = r#"
stages:
  - build

job:
  stage: build
  extends: .missing
"#;

    let err = PipelineGraph::from_yaml_str(yaml).expect_err("unknown template must error");
    assert!(err.to_string().contains("unknown job/template '.missing'"));
}

fn find_job(graph: &PipelineGraph, name: &str) -> NodeIndex {
    graph
        .graph
        .node_indices()
        .find(|&idx| graph.graph[idx].name == name)
        .expect("job must exist")
}
