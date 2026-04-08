#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OPAL_BIN="${OPAL_BIN:-opal}"
OPAL_TEST_COMMAND="${OPAL_TEST_COMMAND:-run}"
DEFAULT_ARGS="--no-tui --max-parallel-jobs 1"
read -r -a OPAL_ARGS <<<"${OPAL_TEST_ARGS:-$DEFAULT_ARGS}"
LOG_DIR="${REPO_ROOT}/tests-temp/test-pipeline-logs"
TEST_RUN_ID="$(date +%s%N)"
mkdir -p "${LOG_DIR}"
export OPAL_HOME="${OPAL_HOME:-${REPO_ROOT}/tests-temp/opal-home}"

if [[ "${OPAL_BIN}" == */* && "${OPAL_BIN}" != /* ]]; then
  OPAL_BIN="${REPO_ROOT}/${OPAL_BIN}"
fi

SCENARIOS_JSON='[
  {"name":"needs-branch","pipeline":"pipelines/tests/needs-and-artifacts.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"tag-ambiguity","pipeline":"pipelines/tests/tag-ambiguity.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=push CI_COMMIT_TAG=","workdir":"tests-temp/tag-ambiguity-workdir","init_git":"1","git_tags":"v0.1.2 v0.1.3","expect_failure":"multiple tags point at HEAD"},
  {"name":"rules-schedule","pipeline":"pipelines/tests/rules-playground.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=schedule RUN_DELAYED=1","command":"plan","opal_args":""},
  {"name":"rules-force-docs","pipeline":"pipelines/tests/rules-playground.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=push FORCE_DOCS=1","command":"plan","opal_args":""},
  {"name":"rules-compare-to","pipeline":"pipelines/tests/rules-compare-to.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=feature/compare-to CI_PIPELINE_SOURCE=push","workdir":"tests-temp/compare-to-workdir","repo_setup":"compare_to_docs_change","command":"plan","opal_args":""},
  {"name":"job-select-plan","pipeline":"pipelines/tests/needs-and-artifacts.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","command":"plan","opal_args":"--job package-linux"},
  {"name":"needs-plan","pipeline":"pipelines/tests/needs-and-artifacts.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=schedule","command":"plan","opal_args":""},
  {"name":"needs-optional","pipeline":"pipelines/tests/needs-and-artifacts.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push ENABLE_OPTIONAL=1","command":"plan","opal_args":""},
  {"name":"needs-tag","pipeline":"pipelines/tests/needs-and-artifacts.gitlab-ci.yml","env":"CI_COMMIT_TAG=v1.2.3 CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"needs-surface","pipeline":"pipelines/tests/needs-surface.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"includes-inherit","pipeline":"pipelines/tests/includes-and-extends.gitlab-ci.yml","env":"SKIP_INHERIT=1","command":"plan","opal_args":""},
  {"name":"yaml-merge-parity","pipeline":"pipelines/tests/yaml-merge-parity.gitlab-ci.yml","env":"","command":"plan","opal_args":""},
  {"name":"inherit-default-parity","pipeline":"pipelines/tests/inherit-default-parity.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"image-platform-parity","pipeline":"pipelines/tests/image-platform-parity.gitlab-ci.yml","env":"","command":"plan","opal_args":""},
  {"name":"image-platform-runtime","pipeline":"pipelines/tests/image-platform-parity.gitlab-ci.yml","env":"","command":"run","opal_args":"--engine docker"},
  {"name":"services-docker-parity","pipeline":"pipelines/tests/services-docker-parity.gitlab-ci.yml","env":"","command":"plan","opal_args":""},
  {"name":"include-surface","pipeline":"pipelines/tests/include-surface.gitlab-ci.yml","env":"","command":"plan","opal_args":""},
  {"name":"include-remote-unsupported","pipeline":"pipelines/tests/include-remote-unsupported.gitlab-ci.yml","env":"","expect_failure":"include:remote is not supported yet","command":"plan","opal_args":""},
  {"name":"include-template-unsupported","pipeline":"pipelines/tests/include-template-unsupported.gitlab-ci.yml","env":"","expect_failure":"include:template is not supported yet","command":"plan","opal_args":""},
  {"name":"include-component-unsupported","pipeline":"pipelines/tests/include-component-unsupported.gitlab-ci.yml","env":"","expect_failure":"include:component is not supported yet","command":"plan","opal_args":""},
  {"name":"includes-parity","pipeline":"pipelines/tests/includes-parity.gitlab-ci.yml","env":"INCLUDE_DYNAMIC_PATH=/pipelines/tests/includes/dynamic.yml","command":"plan","opal_args":""},
  {"name":"top-level-branch","pipeline":"pipelines/tests/top-level-parity.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=feature/top-level CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"top-level-release-skip","pipeline":"pipelines/tests/top-level-parity.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=release/1.0 CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"only-except-schedule","pipeline":"pipelines/tests/only-except-sources.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=schedule","command":"plan","opal_args":""},
  {"name":"only-except-mr","pipeline":"pipelines/tests/only-except-sources.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=merge_request_event","command":"plan","opal_args":""},
  {"name":"only-except-api","pipeline":"pipelines/tests/only-except-sources.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=api","command":"plan","opal_args":""},
  {"name":"only-except-variables","pipeline":"pipelines/tests/only-except-variables.gitlab-ci.yml","env":"RELEASE=staging STAGING=1 SKIP_THIS=0","command":"plan","opal_args":""},
  {"name":"resources-services","pipeline":"pipelines/tests/resources-and-services.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"resource-group-cross-run","pipeline":"pipelines/tests/resource-group-cross-run.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"resources-plan","pipeline":"pipelines/tests/resources-and-services.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"services-and-tags","pipeline":"pipelines/tests/services-and-tags.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"services-default-aliases","pipeline":"pipelines/tests/services-default-aliases.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"services-network-reachability","pipeline":"pipelines/tests/services-network-reachability.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"services-application-connectivity","pipeline":"pipelines/tests/services-application-connectivity.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"services-multi-alias-reachability","pipeline":"pipelines/tests/services-multi-alias-reachability.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"services-network-isolation","pipeline":"pipelines/tests/services-network-isolation.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"services-slow-start","pipeline":"pipelines/tests/services-slow-start.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"services-docker-runtime","pipeline":"pipelines/tests/services-docker-runtime.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","command":"run","opal_args":"--engine docker"},
  {"name":"services-variables","pipeline":"pipelines/tests/services-variables.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"services-invalid-alias","pipeline":"pipelines/tests/services-invalid-alias.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","expect_failure":"unsupported characters"},
  {"name":"runtime-preservation","pipeline":"pipelines/tests/runtime-preservation.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","workdir":"tests-temp/runtime-preservation-workdir","repo_setup":"preserve_runtime","command":"run","opal_args":"--engine docker"},
  {"name":"control-flow-plan","pipeline":"pipelines/tests/control-flow-parity.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"control-flow-runtime","pipeline":"pipelines/tests/control-flow-parity.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","expect_failure":"intentional-failure"},
  {"name":"job-select-runtime","pipeline":"pipelines/tests/control-flow-parity.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","opal_args":"--no-tui --max-parallel-jobs 1 --job rule-variables"},
  {"name":"services-readiness-failure","pipeline":"pipelines/tests/services-readiness-failure.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push OPAL_SERVICE_READY_TIMEOUT_SECS=5","expect_failure":"failed readiness check"},
  {"name":"cache-policies","pipeline":"pipelines/tests/cache-policies.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"cache-key-files","pipeline":"pipelines/tests/cache-key-files.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"cache-fallback","pipeline":"pipelines/tests/cache-fallback.gitlab-ci.yml","env":""},
  {"name":"artifact-metadata-plan","pipeline":"pipelines/tests/artifact-metadata.gitlab-ci.yml","env":"CI_COMMIT_REF_NAME=feature/meta CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"artifact-metadata","pipeline":"pipelines/tests/artifact-metadata.gitlab-ci.yml","env":"CI_COMMIT_REF_NAME=feature/meta CI_PIPELINE_SOURCE=push"},
  {"name":"job-overrides-arch","pipeline":"pipelines/tests/job-overrides-arch.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","workdir":"tests-temp/job-overrides-arch-workdir","repo_setup":"job_override_arch","command":"run","opal_args":"--engine container"},
  {"name":"job-overrides-capabilities","pipeline":"pipelines/tests/job-overrides-capabilities.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","workdir":"tests-temp/job-overrides-cap-workdir","repo_setup":"job_override_caps","command":"run","opal_args":"--engine docker"},
  {"name":"dotenv-reports","pipeline":"pipelines/tests/dotenv-reports.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"retry-parity","pipeline":"pipelines/tests/retry-parity.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push OPAL_HOME=tests-temp/opal-home"},
  {"name":"interruptible-abort","pipeline":"pipelines/tests/interruptible-abort.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push OPAL_ABORT_AFTER_SECS=1"},
  {"name":"filters-branch","pipeline":"pipelines/tests/filters.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=feature/foo CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"filters-tag","pipeline":"pipelines/tests/filters.gitlab-ci.yml","env":"CI_COMMIT_TAG=v1.2.0 CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"environment-plan","pipeline":"pipelines/tests/environments.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"environment-stop","pipeline":"pipelines/tests/environments.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"secret-masking","pipeline":"pipelines/tests/secret-masking.gitlab-ci.yml","env":"","workdir":"tests-temp/secret-masking-workdir","secret_name":"API_TOKEN","secret_value":"super-secret-e2e"}
]'

SELECTED_NAMES=("$@")
if (( ${#SELECTED_NAMES[@]} > 0 )); then
  names_json=$(printf '%s\n' "${SELECTED_NAMES[@]}" | jq -R . | jq -s .)
  FILTERED_SCENARIOS=$(jq --argjson names "${names_json}" '[ .[] | select(.name as $name | $names | index($name)) ]' <<<"${SCENARIOS_JSON}")
else
  FILTERED_SCENARIOS="${SCENARIOS_JSON}"
fi

ACTIVE_SCENARIOS=()
while IFS= read -r line; do
  ACTIVE_SCENARIOS+=("${line}")
done < <(jq -c '.[]' <<<"${FILTERED_SCENARIOS}")

if (( ${#ACTIVE_SCENARIOS[@]} == 0 )); then
  echo "!! No matching scenarios found." >&2
  exit 1
fi

failures=()

detect_runtime_engine() {
  if [[ "$(uname -s)" == "Darwin" ]]; then
    if container system status >/dev/null 2>&1; then
      echo container
      return 0
    fi
    if docker info >/dev/null 2>&1; then
      echo docker
      return 0
    fi
    if podman info >/dev/null 2>&1; then
      echo podman
      return 0
    fi
    if nerdctl info >/dev/null 2>&1; then
      echo nerdctl
      return 0
    fi
    return 1
  fi
  if podman info >/dev/null 2>&1; then
    echo podman
    return 0
  fi
  if docker info >/dev/null 2>&1; then
    echo docker
    return 0
  fi
  if nerdctl info >/dev/null 2>&1; then
    echo nerdctl
    return 0
  fi
  if container system status >/dev/null 2>&1; then
    echo container
    return 0
  fi
  return 1
}

opal_args_include_engine() {
  local arg
  for arg in "${OPAL_ARGS[@]}"; do
    if [[ "${arg}" == "--engine" || "${arg}" == --engine=* ]]; then
      return 0
    fi
  done
  return 1
}

active_scenarios_require_runtime() {
  local scenario effective_command
  for scenario in "${ACTIVE_SCENARIOS[@]}"; do
    effective_command="$(jq -r '.command // empty' <<<"${scenario}")"
    if [[ -z "${effective_command}" ]]; then
      effective_command="${OPAL_TEST_COMMAND}"
    fi
    if [[ "${effective_command}" != "plan" ]]; then
      return 0
    fi
  done
  return 1
}

if active_scenarios_require_runtime && [[ "${OPAL_TEST_COMMAND}" == "run" ]] && ! opal_args_include_engine; then
  if detected_engine="$(detect_runtime_engine)"; then
    OPAL_ARGS+=("--engine" "${detected_engine}")
  else
    echo "!! No usable container runtime found for opal run e2e tests." >&2
    echo "   Tried: docker, podman, nerdctl, container (with system service running)." >&2
    exit 1
  fi
fi

assert_log_contains() {
  local log_file="$1"
  local needle="$2"
  if ! grep -Fq -- "${needle}" "${log_file}"; then
    echo "!! expected log to contain: ${needle}" >&2
    return 1
  fi
}

assert_log_not_contains() {
  local log_file="$1"
  local needle="$2"
  if grep -Fq -- "${needle}" "${log_file}"; then
    echo "!! expected log to not contain: ${needle}" >&2
    return 1
  fi
}

verify_scenario_log() {
  local name="$1"
  local log_file="$2"

  case "${name}" in
    needs-branch)
      assert_log_contains "${log_file}" "artifact exclude ok"
      assert_log_contains "${log_file}" "artifact untracked ok"
      ;;
    filters-branch)
      assert_log_contains "${log_file}" "only-branches"
      assert_log_not_contains "${log_file}" "tag-only"
      ;;
    filters-tag)
      assert_log_contains "${log_file}" "tag-only"
      assert_log_not_contains "${log_file}" "only-branches"
      ;;
    environment-stop)
      assert_log_contains "${log_file}" "deploy-review"
      assert_log_contains "${log_file}" "manual job (env: review/main)"
      assert_log_contains "${log_file}" "env: review/main, url: https://example.com/review/main"
      assert_log_not_contains "${log_file}" 'stopping review env'
      assert_log_not_contains "${log_file}" '${CI_COMMIT_REF_SLUG:-local}'
      ;;
    secret-masking)
      assert_log_contains "${log_file}" "env token=[MASKED]"
      assert_log_contains "${log_file}" "file token=[MASKED]"
      assert_log_not_contains "${log_file}" "super-secret-e2e"
      ;;
    cache-policies)
      assert_log_contains "${log_file}" "cache mutated in pull-only job"
      assert_log_contains "${log_file}" "cache policy seed"
      assert_log_not_contains "${log_file}" "cache policy mutated"
      ;;
    cache-key-files)
      assert_log_contains "${log_file}" "seeded files cache"
      assert_log_contains "${log_file}" "files cache key from lockfile"
      ;;
    cache-fallback)
      assert_log_contains "${log_file}" "seeded default branch cache"
      assert_log_contains "${log_file}" "fallback cache main-seed"
      ;;
    artifact-metadata-plan)
      assert_log_contains "${log_file}" 'name $CI_COMMIT_REF_NAME-artifacts'
      assert_log_contains "${log_file}" "expire_in 2h"
      assert_log_contains "${log_file}" "reports:dotenv tests-temp/meta/build.env"
      ;;
    artifact-metadata)
      assert_log_contains "${log_file}" "artifact metadata consumed v2.0.0"
      ;;
    job-overrides-arch)
      assert_log_contains "${log_file}" "job override arch ok"
      ;;
    job-overrides-capabilities)
      assert_log_contains "${log_file}" "job override caps ok"
      ;;
    dotenv-reports)
      assert_log_contains "${log_file}" "needs dotenv v1.2.3"
      assert_log_contains "${log_file}" "dependencies dotenv v1.2.3"
      assert_log_contains "${log_file}" "dotenv blocked by needs artifacts false"
      assert_log_contains "${log_file}" "dotenv blocked by empty dependencies"
      ;;
    rules-schedule)
      assert_log_contains "${log_file}" "scheduled-maintenance"
      assert_log_contains "${log_file}" "allow failure"
      assert_log_contains "${log_file}" "delayed-verifier"
      assert_log_contains "${log_file}" "start after 10s"
      ;;
    rules-force-docs)
      assert_log_contains "${log_file}" "always-visible"
      assert_log_contains "${log_file}" "docs-or-flag"
      assert_log_contains "${log_file}" "when manual"
      assert_log_not_contains "${log_file}" "delayed-verifier"
      ;;
    rules-compare-to)
      assert_log_contains "${log_file}" "docs-compare-to"
      assert_log_not_contains "${log_file}" "src-compare-to"
      ;;
    job-select-plan)
      assert_log_contains "${log_file}" "package-linux"
      assert_log_contains "${log_file}" "prepare-artifacts"
      assert_log_contains "${log_file}" "build-matrix: [linux, release]"
      assert_log_not_contains "${log_file}" "smoke-tests"
      assert_log_not_contains "${log_file}" "deploy-summary"
      ;;
    needs-plan)
      assert_log_contains "${log_file}" "build-matrix: [linux, debug]"
      assert_log_contains "${log_file}" "build-matrix: [mac, release]"
      assert_log_contains "${log_file}" "when delayed"
      assert_log_contains "${log_file}" 'environment: review/${CI_COMMIT_REF_SLUG:-local}'
      ;;
    needs-optional)
      assert_log_contains "${log_file}" "optional-smoke"
      assert_log_contains "${log_file}" "optional: yes"
      ;;
    needs-tag)
      assert_log_contains "${log_file}" "tagged-release"
      assert_log_contains "${log_file}" "when manual"
      assert_log_not_contains "${log_file}" "delayed-rollout"
      ;;
    needs-surface)
      assert_log_contains "${log_file}" "consumer-no-artifacts"
      assert_log_contains "${log_file}" "artifacts requested: no"
      assert_log_contains "${log_file}" "consumer-targeted-matrix"
      assert_log_contains "${log_file}" "matrix filters"
      ;;
    includes-inherit)
      assert_log_contains "${log_file}" "lint-job"
      assert_log_contains "${log_file}" "no-inherit-build"
      assert_log_contains "${log_file}" "direct-job"
      ;;
    yaml-merge-parity)
      assert_log_contains "${log_file}" "merged-job"
      ;;
    inherit-default-parity)
      assert_log_contains "${log_file}" "inherit-none"
      assert_log_contains "${log_file}" "image: docker.io/library/alpine:3.19"
      assert_log_contains "${log_file}" "inherit-some"
      assert_log_contains "${log_file}" "retries 2"
      assert_log_contains "${log_file}" "interruptible"
      assert_log_not_contains "${log_file}" "default-cache"
      ;;
    image-platform-parity)
      assert_log_contains "${log_file}" "platform-job"
      assert_log_contains "${log_file}" "platform: linux/arm64/v8"
      assert_log_contains "${log_file}" "user: 1000:1000"
      assert_log_contains "${log_file}" "entrypoint: []"
      ;;
    image-platform-runtime)
      assert_log_contains "${log_file}" "platform job"
      ;;
    services-docker-parity)
      assert_log_contains "${log_file}" "service-options"
      assert_log_contains "${log_file}" "services: • docker.io/library/redis:7.2"
      assert_log_contains "${log_file}" "alias cache"
      assert_log_contains "${log_file}" "platform linux/arm64/v8"
      assert_log_contains "${log_file}" "user 1000:1000"
      ;;
    include-surface)
      assert_log_contains "${log_file}" "root-fragment-job"
      assert_log_contains "${log_file}" "list-one-job"
      assert_log_contains "${log_file}" "include-surface-main"
      ;;
    retry-parity)
      assert_log_contains "${log_file}" "first script failure"
      assert_log_contains "${log_file}" "retry-script-failure-ok"
      assert_log_contains "${log_file}" "first exit 137"
      assert_log_contains "${log_file}" "retry-exit-codes-ok"
      assert_log_contains "${log_file}" "retry fixture complete"
      ;;
    interruptible-abort)
      assert_log_contains "${log_file}" "interruptible start"
      assert_log_contains "${log_file}" "noninterruptible start"
      assert_log_contains "${log_file}" "noninterruptible done"
      assert_log_not_contains "${log_file}" '0006] interruptible done'
      assert_log_not_contains "${log_file}" "should not run after abort"
      ;;
    resources-plan)
      assert_log_contains "${log_file}" "retries 2"
      assert_log_contains "${log_file}" "interruptible"
      assert_log_contains "${log_file}" "timeout: 10m"
      assert_log_contains "${log_file}" "resource group: prod-lock"
      assert_log_contains "${log_file}" "environment: review/resources"
      ;;
    resource-group-cross-run)
      assert_log_contains "${log_file}" "lock holder start"
      assert_log_contains "${log_file}" "lock holder done"
      ;;
    top-level-branch)
      assert_log_contains "${log_file}" "top-level-job"
      assert_log_contains "${log_file}" 'caches: • key $CI_COMMIT_REF_SLUG'
      ;;
    top-level-release-skip)
      assert_log_contains "${log_file}" "pipeline skipped: top-level only/except filters exclude this ref"
      ;;
    only-except-schedule)
      assert_log_contains "${log_file}" "schedule-only"
      assert_log_not_contains "${log_file}" "except-schedules"
      assert_log_not_contains "${log_file}" "push-only"
      ;;
    only-except-mr)
      assert_log_contains "${log_file}" "mr-only"
      assert_log_contains "${log_file}" "except-schedules"
      assert_log_not_contains "${log_file}" "schedule-only"
      ;;
    only-except-api)
      assert_log_contains "${log_file}" "api-only"
      assert_log_contains "${log_file}" "except-schedules"
      assert_log_not_contains "${log_file}" "mr-only"
      ;;
    only-except-variables)
      assert_log_contains "${log_file}" "release-only"
      assert_log_contains "${log_file}" "flag-only"
      assert_log_contains "${log_file}" "skip-when-disabled"
      ;;
    services-and-tags)
      assert_log_contains "${log_file}" "services: • docker.io/library/postgres:16"
      assert_log_contains "${log_file}" "alias cache,redis"
      assert_log_contains "${log_file}" "entrypoint [redis-server]"
      assert_log_contains "${log_file}" "command [--save, , --appendonly, no]"
      assert_log_contains "${log_file}" "variables REDIS_PASSWORD"
      assert_log_contains "${log_file}" "tags: • local-shell, docker-large"
      ;;
    services-default-aliases)
      assert_log_contains "${log_file}" "default aliases ok"
      ;;
    services-network-reachability)
      assert_log_contains "${log_file}" "service alias reachability ok"
      ;;
    services-application-connectivity)
      assert_log_contains "${log_file}" "service application connectivity ok"
      ;;
    services-multi-alias-reachability)
      assert_log_contains "${log_file}" "service multi alias reachability ok"
      ;;
    services-network-isolation)
      assert_log_contains "${log_file}" "first job service reachable"
      assert_log_contains "${log_file}" "service network isolation ok"
      assert_log_not_contains "${log_file}" '0003] service leaked across jobs'
      ;;
    services-slow-start)
      assert_log_contains "${log_file}" "service slow start ok"
      ;;
    services-docker-runtime)
      assert_log_contains "${log_file}" "service docker runtime ok"
      ;;
    services-variables)
      assert_log_contains "${log_file}" "service variables ok"
      ;;
    runtime-preservation)
      assert_log_contains "${log_file}" "runtime preservation ok"
      local latest_run
      latest_run=$(jq -r '.[-1].run_id' tests-temp/opal-home/history.json)
      jq -e '.[-1].jobs[] | select(.name == "preserved-runtime") | .container_name and .service_network and (.service_containers | length > 0) and .runtime_summary_path' tests-temp/opal-home/history.json >/dev/null
      local summary_path
      summary_path=$(jq -r '.[-1].jobs[] | select(.name == "preserved-runtime") | .runtime_summary_path' tests-temp/opal-home/history.json)
      test -f "$summary_path"
      grep -Fq 'Main container' "$summary_path"
      grep -Fq 'Service containers' "$summary_path"
      ;;
    environment-plan)
      assert_log_contains "${log_file}" "on_stop: stop-review"
      assert_log_contains "${log_file}" "auto_stop 1day"
      assert_log_contains "${log_file}" 'environment: review/${CI_COMMIT_REF_SLUG:-local} – stop'
      assert_log_contains "${log_file}" 'environment: review/${CI_COMMIT_REF_SLUG:-local} – prepare'
      assert_log_contains "${log_file}" 'environment: review/${CI_COMMIT_REF_SLUG:-local} – verify'
      assert_log_contains "${log_file}" 'environment: review/${CI_COMMIT_REF_SLUG:-local} – access'
      ;;
    control-flow-plan)
      assert_log_contains "${log_file}" "parallel-fanout: [1]"
      assert_log_contains "${log_file}" "parallel-fanout: [2]"
      assert_log_contains "${log_file}" "when on_failure"
      ;;
    job-select-runtime)
      assert_log_contains "${log_file}" "rule variable from-rule"
      assert_log_not_contains "${log_file}" "parallel-fanout 1/2"
      assert_log_not_contains "${log_file}" "intentional failure"
      ;;
    control-flow-runtime)
      assert_log_contains "${log_file}" "rule variable from-rule"
      assert_log_contains "${log_file}" "intentional failure"
      assert_log_contains "${log_file}" "on failure top-level-variable"
      ;;
    includes-parity)
      assert_log_contains "${log_file}" "root-fragment-job"
      assert_log_contains "${log_file}" "glob-alpha-job"
      assert_log_contains "${log_file}" "glob-bravo-job"
      assert_log_contains "${log_file}" "list-one-job"
      assert_log_contains "${log_file}" "list-two-job"
      assert_log_contains "${log_file}" "dynamic-include-job"
      assert_log_contains "${log_file}" "main-include-job"
      ;;
  esac
}

prepare_scenario_workdir() {
  local workdir="$1"
  local secret_name="$2"
  local secret_value="$3"
  local init_git="$4"
  local git_tags="$5"
  local repo_setup="$6"

  mkdir -p "${workdir}"
  if [[ -n "${repo_setup}" ]]; then
    rm -rf "${workdir}"
    mkdir -p "${workdir}"
    case "${repo_setup}" in
      compare_to_docs_change)
        mkdir -p "${workdir}/docs" "${workdir}/src"
        git -C "${workdir}" init -b main >/dev/null
        printf '# Guide\nbase\n' > "${workdir}/docs/guide.md"
        printf 'fn main() {}\n' > "${workdir}/src/main.rs"
        git -C "${workdir}" add docs/guide.md src/main.rs
        git -C "${workdir}" -c user.name='Opal Tests' -c user.email='opal@example.com' commit -m 'base' >/dev/null
        git -C "${workdir}" checkout -b feature/compare-to >/dev/null
        printf '# Guide\nchanged\n' > "${workdir}/docs/guide.md"
        git -C "${workdir}" add docs/guide.md
        git -C "${workdir}" -c user.name='Opal Tests' -c user.email='opal@example.com' commit -m 'docs change' >/dev/null
        ;;
      job_override_arch)
        mkdir -p "${workdir}/.opal"
        cat > "${workdir}/.opal/config.toml" <<'TOML'
[[jobs]]
name = "arm-job"
arch = "arm64"
TOML
        ;;
      job_override_caps)
        mkdir -p "${workdir}/.opal"
        cat > "${workdir}/.opal/config.toml" <<'TOML'
[[jobs]]
name = "cap-job"
cap_add = ["NET_ADMIN"]
TOML
        ;;
      preserve_runtime)
        mkdir -p "${workdir}/.opal"
        cat > "${workdir}/.opal/config.toml" <<'TOML'
[engine]
preserve_runtime_objects = true
TOML
        ;;
      *)
        echo "!! unknown repo setup: ${repo_setup}" >&2
        return 1
        ;;
    esac
    return 0
  fi
  if [[ "${init_git}" == "1" ]]; then
    git -C "${workdir}" init -b main >/dev/null
    printf 'opal\n' > "${workdir}/README.md"
    git -C "${workdir}" add README.md
    git -C "${workdir}" -c user.name='Opal Tests' -c user.email='opal@example.com' commit -m 'initial' >/dev/null
    local tag
    for tag in ${git_tags}; do
      git -C "${workdir}" tag "${tag}"
    done
  fi
  if [[ -n "${secret_name}" ]]; then
    local secrets_dir="${workdir}/.opal/env"
    mkdir -p "${secrets_dir}"
    printf '%s' "${secret_value}" > "${secrets_dir}/${secret_name}"
  fi
}

run_scenario() {
  local name="$1"
  local pipeline_rel="$2"
  local env_string="$3"
  local workdir_rel="$4"
  local secret_name="$5"
  local secret_value="$6"
  local init_git="$7"
  local git_tags="$8"
  local expect_failure="$9"
  local scenario_command="${10}"
  local scenario_opal_args="${11}"
  local repo_setup="${12}"
  local pipeline_path="${REPO_ROOT}/${pipeline_rel}"
  local log_name="${name//[^A-Za-z0-9._-]/_}"
  local log_file="${LOG_DIR}/${log_name}.log"
  local workdir="${REPO_ROOT}"

  if [[ -n "${workdir_rel}" ]]; then
    workdir="${REPO_ROOT}/${workdir_rel}"
  fi
  if [[ "${init_git}" == "1" ]]; then
    workdir="${workdir}-${TEST_RUN_ID}"
  fi

  if [[ ! -f "${pipeline_path}" ]]; then
    echo "!! ${name}: pipeline not found at ${pipeline_rel}" >&2
    return 1
  fi

  prepare_scenario_workdir "${workdir}" "${secret_name}" "${secret_value}" "${init_git}" "${git_tags}" "${repo_setup}"

  echo "==> ${name}"
  pushd "${workdir}" >/dev/null

  local effective_command="${OPAL_TEST_COMMAND}"
  if [[ -n "${scenario_command}" ]]; then
    effective_command="${scenario_command}"
  fi

  local cmd=("${OPAL_BIN}" "${effective_command}")
  if [[ "${effective_command}" == "plan" ]]; then
    if [[ "${scenario_opal_args}" != "__DEFAULT__" ]]; then
      local scenario_args=()
      read -r -a scenario_args <<<"${scenario_opal_args}"
      if [[ ${#scenario_args[@]} -gt 0 ]]; then
        cmd+=("${scenario_args[@]}")
      fi
    fi
  elif [[ "${scenario_opal_args}" == "__DEFAULT__" ]]; then
    if [[ ${#OPAL_ARGS[@]} -gt 0 && -n "${OPAL_ARGS[0]}" ]]; then
      cmd+=("${OPAL_ARGS[@]}")
    fi
  elif [[ -n "${scenario_opal_args}" ]]; then
    if [[ ${#OPAL_ARGS[@]} -gt 0 && -n "${OPAL_ARGS[0]}" ]]; then
      cmd+=("${OPAL_ARGS[@]}")
    fi
    local scenario_args=()
    read -r -a scenario_args <<<"${scenario_opal_args}"
    if [[ ${#scenario_args[@]} -gt 0 ]]; then
      cmd+=("${scenario_args[@]}")
    fi
  fi
  cmd+=("--workdir" "${workdir}")
  cmd+=("--pipeline" "${pipeline_path}")

  if [[ -n "${env_string}" ]]; then
    # shellcheck disable=SC2086
    env ${env_string} "${cmd[@]}" 2>&1 | tee "${log_file}"
  else
    "${cmd[@]}" 2>&1 | tee "${log_file}"
  fi
  local status=$?
  popd >/dev/null

  if [[ -n "${expect_failure}" ]]; then
    if (( status == 0 )); then
      echo "!! ${name}: expected failure but scenario succeeded" >&2
      echo "    log saved to ${log_file} (verification failed)"
      return 1
    fi
    if ! assert_log_contains "${log_file}" "${expect_failure}"; then
      echo "    log saved to ${log_file} (verification failed)"
      return 1
    fi
    if ! verify_scenario_log "${name}" "${log_file}"; then
      echo "    log saved to ${log_file} (verification failed)"
      return 1
    fi
    echo "    log saved to ${log_file} (expected failure)"
    return 0
  fi

  if (( status == 0 )); then
    if ! verify_scenario_log "${name}" "${log_file}"; then
      echo "    log saved to ${log_file} (verification failed)"
      return 1
    fi
    echo "    log saved to ${log_file}"
  else
    echo "    log saved to ${log_file} (failed)"
  fi

  return ${status}
}

run_cache_fallback_scenario() {
  local pipeline_rel="$1"
  local pipeline_path="${REPO_ROOT}/${pipeline_rel}"
  local log_file="${LOG_DIR}/cache-fallback.log"
  local namespace="cache-fallback-${TEST_RUN_ID}"
  local common_env="CI_PIPELINE_SOURCE=push CI_DEFAULT_BRANCH=main CI_CACHE_NAMESPACE=${namespace}"
  local cache_root="${OPAL_HOME:-${HOME}/.opal}/cache"

  : > "${log_file}"

  local env_string
  local run_index=0
  for env_string in \
    "CI_COMMIT_BRANCH=main ${common_env}" \
    "CI_COMMIT_BRANCH=feature/fallback ${common_env}"
  do
    run_index=$((run_index + 1))
    local cmd=("${OPAL_BIN}" "${OPAL_TEST_COMMAND}")
    if [[ ${#OPAL_ARGS[@]} -gt 0 && -n "${OPAL_ARGS[0]}" ]]; then
      cmd+=("${OPAL_ARGS[@]}")
    fi
    cmd+=("--workdir" "${REPO_ROOT}")
    cmd+=("--pipeline" "${pipeline_path}")

    pushd "${REPO_ROOT}" >/dev/null
    # shellcheck disable=SC2086
    env ${env_string} "${cmd[@]}" 2>&1 | tee -a "${log_file}"
    local status=${PIPESTATUS[0]}
    popd >/dev/null
    if (( status != 0 )); then
      echo "    log saved to ${log_file} (failed)"
      return ${status}
    fi
    if (( run_index == 1 )); then
      if ! wait_for_seeded_cache "${cache_root}" "${namespace}"; then
        echo "    log saved to ${log_file} (verification failed)"
        return 1
      fi
    fi
  done

  if ! verify_scenario_log "cache-fallback" "${log_file}"; then
    echo "    log saved to ${log_file} (verification failed)"
    return 1
  fi
  echo "    log saved to ${log_file}"
  return 0
}

run_resource_group_cross_run_scenario() {
  local pipeline_rel="$1"
  local pipeline_path="${REPO_ROOT}/${pipeline_rel}"
  local log_file="${LOG_DIR}/resource-group-cross-run.log"

  : > "${log_file}"

  local cmd=("${OPAL_BIN}" "run")
  if [[ ${#OPAL_ARGS[@]} -gt 0 && -n "${OPAL_ARGS[0]}" ]]; then
    cmd+=("${OPAL_ARGS[@]}")
  fi
  cmd+=("--workdir" "${REPO_ROOT}")
  cmd+=("--pipeline" "${pipeline_path}")

  pushd "${REPO_ROOT}" >/dev/null
  local start_ts
  start_ts=$(date +%s)
  env CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push "${cmd[@]}" > "${log_file}.first" 2>&1 &
  local first_pid=$!
  sleep 1
  env CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push "${cmd[@]}" > "${log_file}.second" 2>&1
  local second_status=$?
  wait ${first_pid}
  local first_status=$?
  local end_ts
  end_ts=$(date +%s)
  cat "${log_file}.first" "${log_file}.second" > "${log_file}"
  rm -f "${log_file}.first" "${log_file}.second"
  popd >/dev/null

  if (( first_status != 0 || second_status != 0 )); then
    echo "    log saved to ${log_file} (failed)"
    return 1
  fi
  if (( end_ts - start_ts < 5 )); then
    echo "!! expected cross-run resource_group serialization to delay total runtime" >&2
    echo "    log saved to ${log_file} (verification failed)"
    return 1
  fi
  if ! verify_scenario_log "resource-group-cross-run" "${log_file}"; then
    echo "    log saved to ${log_file} (verification failed)"
    return 1
  fi
  echo "    log saved to ${log_file}"
  return 0
}

run_interruptible_abort_scenario() {
  local pipeline_rel="$1"
  local env_string="$2"
  local pipeline_path="${REPO_ROOT}/${pipeline_rel}"
  local log_file="${LOG_DIR}/interruptible-abort.log"

  : > "${log_file}"

  local cmd=("${OPAL_BIN}" "run")
  if [[ ${#OPAL_ARGS[@]} -gt 0 && -n "${OPAL_ARGS[0]}" ]]; then
    cmd+=("${OPAL_ARGS[@]}")
  fi
  cmd+=("--max-parallel-jobs" "2")
  cmd+=("--workdir" "${REPO_ROOT}")
  cmd+=("--pipeline" "${pipeline_path}")

  pushd "${REPO_ROOT}" >/dev/null
  # shellcheck disable=SC2086
  env ${env_string} "${cmd[@]}" > "${log_file}" 2>&1 &
  local run_pid=$!

  local attempt
  for attempt in {1..30}; do
    if grep -Fq "noninterruptible start" "${log_file}" 2>/dev/null && grep -Fq "interruptible start" "${log_file}" 2>/dev/null; then
      break
    fi
    sleep 0.5
  done

  kill -INT ${run_pid} >/dev/null 2>&1 || true
  wait ${run_pid}
  local status=$?
  popd >/dev/null

  if (( status == 0 )); then
    echo "!! interruptible-abort: expected abort-driven failure exit" >&2
    echo "    log saved to ${log_file} (verification failed)"
    return 1
  fi
  if ! verify_scenario_log "interruptible-abort" "${log_file}"; then
    echo "    log saved to ${log_file} (verification failed)"
    return 1
  fi
  echo "    log saved to ${log_file} (expected failure)"
  return 0
}

wait_for_seeded_cache() {
  local cache_root="$1"
  local namespace="$2"

  local attempt
  for attempt in {1..10}; do
    if find "${cache_root}" -maxdepth 4 -path "*/${namespace}-main-*/tests-temp/cache-data/fallback.txt" -print -quit | grep -q .; then
      return 0
    fi
    sleep 1
  done

  echo "!! expected seeded fallback cache for namespace ${namespace}" >&2
  return 1
}

for entry in "${ACTIVE_SCENARIOS[@]}"; do
  name=$(jq -r '.name' <<<"${entry}")
  pipeline=$(jq -r '.pipeline' <<<"${entry}")
  envs=$(jq -r '.env' <<<"${entry}")
  workdir=$(jq -r '.workdir // ""' <<<"${entry}")
  secret_name=$(jq -r '.secret_name // ""' <<<"${entry}")
  secret_value=$(jq -r '.secret_value // ""' <<<"${entry}")
  init_git=$(jq -r '.init_git // "0"' <<<"${entry}")
  git_tags=$(jq -r '.git_tags // ""' <<<"${entry}")
  repo_setup=$(jq -r '.repo_setup // ""' <<<"${entry}")
  expect_failure=$(jq -r '.expect_failure // ""' <<<"${entry}")
  scenario_command=$(jq -r '.command // ""' <<<"${entry}")
  scenario_opal_args=$(jq -r '.opal_args // "__DEFAULT__"' <<<"${entry}")
  if [[ "${name}" == "cache-fallback" ]]; then
    echo "==> ${name}"
    if ! run_cache_fallback_scenario "${pipeline}"; then
      failures+=("${name}")
    fi
    continue
  fi
  if [[ "${name}" == "resource-group-cross-run" ]]; then
    echo "==> ${name}"
    if ! run_resource_group_cross_run_scenario "${pipeline}"; then
      failures+=("${name}")
    fi
    continue
  fi
  if [[ "${name}" == "interruptible-abort" ]]; then
    echo "==> ${name}"
    if ! run_interruptible_abort_scenario "${pipeline}" "${envs}"; then
      failures+=("${name}")
    fi
    continue
  fi
  if ! run_scenario "${name}" "${pipeline}" "${envs}" "${workdir}" "${secret_name}" "${secret_value}" "${init_git}" "${git_tags}" "${expect_failure}" "${scenario_command}" "${scenario_opal_args}" "${repo_setup}"; then
    failures+=("${name}")
  fi
done

if (( ${#failures[@]} > 0 )); then
  echo "!! Test pipeline failures: ${failures[*]}" >&2
  exit 1
fi

echo "✅ All test pipelines completed successfully."
