#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OPAL_BIN_REQUESTED="${OPAL_BIN:-opal}"
OPAL_BIN=""
OPAL_TEST_COMMAND="${OPAL_TEST_COMMAND:-run}"
DEFAULT_ARGS="--no-tui"
read -r -a OPAL_ARGS <<<"${OPAL_TEST_ARGS:-$DEFAULT_ARGS}"
export PATH="/usr/local/bin:/opt/homebrew/bin:${PATH}"
TEST_RUN_ID="$(date +%s%N)"
TMP_PARENT="${OPAL_TMP_TEST_ROOT:-/tmp}"
mkdir -p "${TMP_PARENT}"
TMP_RUN_ROOT="$(mktemp -d "${TMP_PARENT%/}/opal-test-pipelines-${TEST_RUN_ID}-XXXXXX")"
LOG_DIR="${TMP_RUN_ROOT}/logs"
ARTIFACT_LOG_DIR="${REPO_ROOT}/tests-temp/test-pipeline-logs"
GIT_TEMPLATE_DIR_OPAL="${TMP_RUN_ROOT}/git-template"
GIT_STATE_ROOT="${TMP_RUN_ROOT}/git-state"
RUNTIME_WORKDIR_ROOT="${TMP_RUN_ROOT}/workdirs"
GIT_ENV_UNSET=(
  -u GIT_DIR
  -u GIT_WORK_TREE
  -u GIT_COMMON_DIR
  -u GIT_INDEX_FILE
  -u GIT_OBJECT_DIRECTORY
  -u GIT_ALTERNATE_OBJECT_DIRECTORIES
  -u GIT_CEILING_DIRECTORIES
)
SCENARIO_CI_UNSET=(
  -u CI_COMMIT_REF_NAME
  -u CI_COMMIT_REF_SLUG
)
mkdir -p "${LOG_DIR}"
mkdir -p "${ARTIFACT_LOG_DIR}"
mkdir -p "${GIT_TEMPLATE_DIR_OPAL}"
mkdir -p "${GIT_STATE_ROOT}"
mkdir -p "${RUNTIME_WORKDIR_ROOT}"
export XDG_DATA_HOME="${TMP_RUN_ROOT}/opal-home"
export RUSTC_WRAPPER=""
export SCCACHE_DISABLE="1"

JSON_BACKEND=""

ensure_json_backend() {
  if command -v jq >/dev/null 2>&1; then
    JSON_BACKEND="jq"
    return 0
  fi
  JSON_BACKEND="bash"
  return 0
}

json_field() {
  local json_entry="$1"
  local field="$2"
  local default_value="${3:-}"
  if [[ "${JSON_BACKEND}" == "jq" ]]; then
    jq -r --arg key "${field}" --arg default "${default_value}" '.[$key] // $default' <<<"${json_entry}"
    return 0
  fi

  local token="\"${field}\":\""
  if [[ "${json_entry}" == *"${token}"* ]]; then
    local rest="${json_entry#*"${token}"}"
    printf '%s\n' "${rest%%\"*}"
    return 0
  fi
  printf '%s\n' "${default_value}"
}

scenario_name_selected() {
  local scenario_name="$1"
  shift || true
  if (( $# == 0 )); then
    return 0
  fi
  local selected
  for selected in "$@"; do
    if [[ "${selected}" == "${scenario_name}" ]]; then
      return 0
    fi
  done
  return 1
}

extract_scenario_entries() {
  local scenarios_json="$1"
  local raw
  while IFS= read -r raw; do
    [[ -z "${raw}" ]] && continue
    printf '{%s}\n' "${raw}"
  done < <(sed -n 's/^[[:space:]]*{\(.*\)}[[:space:]]*,\{0,1\}[[:space:]]*$/\1/p' <<<"${scenarios_json}")
}

json_select_scenarios() {
  local scenarios_json="$1"
  shift || true
  if [[ "${JSON_BACKEND}" == "jq" ]]; then
    if (( $# == 0 )); then
      printf '%s\n' "${scenarios_json}"
      return 0
    fi
    local names_json
    names_json=$(printf '%s\n' "$@" | jq -R . | jq -s .)
    jq --argjson names "${names_json}" '[ .[] | select(.name as $name | $names | index($name)) ]' <<<"${scenarios_json}"
    return 0
  fi

  local entry
  while IFS= read -r entry; do
    local name
    name=$(json_field "${entry}" "name")
    if scenario_name_selected "${name}" "$@"; then
      printf '%s\n' "${entry}"
    fi
  done < <(extract_scenario_entries "${scenarios_json}")
}

json_entries() {
  local scenarios_json="$1"
  if [[ "${JSON_BACKEND}" == "jq" ]]; then
    jq -c '.[]' <<<"${scenarios_json}"
    return 0
  fi
  printf '%s\n' "${scenarios_json}"
}

json_latest_run_id() {
  local history_path="$1"
  if [[ "${JSON_BACKEND}" == "jq" ]]; then
    jq -r '.[-1].run_id' "${history_path}"
    return 0
  fi
  local compact
  compact="$(tr -d '\r\n' < "${history_path}")"
  local marker='"run_id":"'
  if [[ "${compact}" != *"${marker}"* ]]; then
    return 1
  fi
  local tail="${compact##*"${marker}"}"
  printf '%s\n' "${tail%%\"*}"
}

json_verify_preserved_runtime_fields() {
  local history_path="$1"
  if [[ "${JSON_BACKEND}" == "jq" ]]; then
    jq -e '.[-1].jobs[] | select(.name == "preserved-runtime") | .container_name and .service_network and (.service_containers | length > 0) and .runtime_summary_path' "${history_path}" >/dev/null
    return 0
  fi
  local compact
  compact="$(tr -d '\r\n' < "${history_path}")"
  local marker='"name":"preserved-runtime"'
  if [[ "${compact}" != *"${marker}"* ]]; then
    return 1
  fi
  local tail="${compact#*"${marker}"}"
  [[ "${tail}" =~ \"container_name\":\"[^\"]+\" ]] || return 1
  [[ "${tail}" =~ \"service_network\":\"[^\"]+\" ]] || return 1
  [[ "${tail}" =~ \"service_containers\":\[[^]]+\] ]] || return 1
  [[ "${tail}" =~ \"runtime_summary_path\":\"[^\"]+\" ]] || return 1
  return 0
}

json_preserved_runtime_summary_path() {
  local history_path="$1"
  if [[ "${JSON_BACKEND}" == "jq" ]]; then
    jq -r '.[-1].jobs[] | select(.name == "preserved-runtime") | .runtime_summary_path' "${history_path}"
    return 0
  fi
  local compact
  compact="$(tr -d '\r\n' < "${history_path}")"
  local marker='"name":"preserved-runtime"'
  if [[ "${compact}" != *"${marker}"* ]]; then
    return 1
  fi
  local tail="${compact#*"${marker}"}"
  local summary_marker='"runtime_summary_path":"'
  if [[ "${tail}" != *"${summary_marker}"* ]]; then
    return 1
  fi
  local summary_tail="${tail#*"${summary_marker}"}"
  printf '%s\n' "${summary_tail%%\"*}"
}

if [[ "${OPAL_BIN_REQUESTED}" == */* && "${OPAL_BIN_REQUESTED}" != /* ]]; then
  OPAL_BIN_REQUESTED="${REPO_ROOT}/${OPAL_BIN_REQUESTED}"
fi

resolve_opal_bin() {
  local requested="${OPAL_BIN_REQUESTED}"
  local candidates=()
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    candidates+=("${CARGO_TARGET_DIR}/debug/opal")
  fi
  candidates+=(
    "${REPO_ROOT}/target/debug/opal"
    "${REPO_ROOT}/target/extended-tests/debug/opal"
    "${REPO_ROOT}/target/e2e-tests/debug/opal"
  )

  if [[ "${requested}" == */* ]]; then
    if [[ -x "${requested}" ]]; then
      OPAL_BIN="${requested}"
      return 0
    fi
    echo "!! requested OPAL_BIN executable not found: ${requested}" >&2
    return 1
  fi

  if [[ "${requested}" != "opal" ]]; then
    echo "!! OPAL_BIN must be a path to a local compiled binary; got command name '${requested}'" >&2
    return 1
  fi

  local candidate
  for candidate in "${candidates[@]}"; do
    if [[ -x "${candidate}" ]]; then
      OPAL_BIN="${candidate}"
      return 0
    fi
  done

  echo "!! unable to resolve local compiled opal binary from expected targets" >&2
  echo "!! looked for:" >&2
  for candidate in "${candidates[@]}"; do
    echo "!!   - ${candidate}" >&2
  done
  echo "!! run 'cargo build -p opal --bin opal --locked' first, or set OPAL_BIN to a compiled binary path" >&2
  return 1
}

resolve_opal_bin

OPAL_BIN="$(cd "$(dirname "${OPAL_BIN}")" && pwd)/$(basename "${OPAL_BIN}")"
if [[ "${OPAL_BIN}" != "${REPO_ROOT}"/* ]]; then
  echo "!! resolved OPAL_BIN must be built from this repository" >&2
  echo "!! repo root: ${REPO_ROOT}" >&2
  echo "!! resolved opal: ${OPAL_BIN}" >&2
  exit 1
fi

if command -v strings >/dev/null 2>&1; then
  if strings "${OPAL_BIN}" | grep -Fq "opal workspace snapshot"; then
    echo "!! resolved OPAL_BIN (${OPAL_BIN}) embeds deprecated synthetic workspace snapshot commits" >&2
    echo "!! build the local repo binary first or set OPAL_BIN to a current build" >&2
    exit 1
  fi
fi

ensure_json_backend
if [[ "${JSON_BACKEND}" == "jq" ]]; then
  echo "==> using jq: $(command -v jq)"
else
  echo "==> using json backend: bash"
fi

SCENARIOS_JSON='[
  {"name":"needs-branch","pipeline":"pipelines/tests/needs-and-artifacts.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"tag-ambiguity","pipeline":"pipelines/tests/tag-ambiguity.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=push CI_COMMIT_TAG=","workdir":"tests-temp/tag-ambiguity-workdir","init_git":"1","git_tags":"v0.1.2 v0.1.3","expect_failure":"multiple tags point at HEAD"},
  {"name":"rules-schedule","pipeline":"pipelines/tests/rules-playground.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=schedule RUN_DELAYED=1","command":"plan","opal_args":""},
  {"name":"rules-force-docs","pipeline":"pipelines/tests/rules-playground.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=push CI_COMMIT_BRANCH=main FORCE_DOCS=1","command":"plan","opal_args":""},
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
  {"name":"job-select-runtime","pipeline":"pipelines/tests/control-flow-parity.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","opal_args":"--no-tui --job rule-variables"},
  {"name":"services-readiness-failure","pipeline":"pipelines/tests/services-readiness-failure.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push OPAL_SERVICE_READY_TIMEOUT_SECS=5","expect_failure":"failed readiness check"},
  {"name":"cache-policies","pipeline":"pipelines/tests/cache-policies.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"cache-key-files","pipeline":"pipelines/tests/cache-key-files.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"cache-fallback","pipeline":"pipelines/tests/cache-fallback.gitlab-ci.yml","env":""},
  {"name":"artifact-metadata-plan","pipeline":"pipelines/tests/artifact-metadata.gitlab-ci.yml","env":"CI_COMMIT_REF_NAME=feature/meta CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"artifact-metadata","pipeline":"pipelines/tests/artifact-metadata.gitlab-ci.yml","env":"CI_COMMIT_REF_NAME=feature/meta CI_PIPELINE_SOURCE=push"},
  {"name":"job-overrides-arch","pipeline":"pipelines/tests/job-overrides-arch.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","workdir":"tests-temp/job-overrides-arch-workdir","repo_setup":"job_override_arch","command":"run","opal_args":"--engine container"},
  {"name":"job-overrides-capabilities","pipeline":"pipelines/tests/job-overrides-capabilities.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","workdir":"tests-temp/job-overrides-cap-workdir","repo_setup":"job_override_caps","command":"run","opal_args":"--engine docker"},
  {"name":"dotenv-reports","pipeline":"pipelines/tests/dotenv-reports.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"bootstrap-runner","pipeline":"pipelines/tests/bootstrap-runner.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","workdir":"tests-temp/bootstrap-runner-workdir","repo_setup":"bootstrap_runner"},
  {"name":"retry-parity","pipeline":"pipelines/tests/retry-parity.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push XDG_DATA_HOME=tests-temp/opal-home"},
  {"name":"interruptible-abort","pipeline":"pipelines/tests/interruptible-abort.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push OPAL_ABORT_AFTER_SECS=1"},
  {"name":"filters-branch","pipeline":"pipelines/tests/filters.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=feature/foo CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"filters-tag","pipeline":"pipelines/tests/filters.gitlab-ci.yml","env":"CI_COMMIT_TAG=v1.2.0 CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"environment-plan","pipeline":"pipelines/tests/environments.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push","command":"plan","opal_args":""},
  {"name":"environment-stop","pipeline":"pipelines/tests/environments.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"secret-masking","pipeline":"pipelines/tests/secret-masking.gitlab-ci.yml","env":"","workdir":"tests-temp/secret-masking-workdir","secret_name":"API_TOKEN","secret_value":"super-secret-e2e"}
]'

SELECTED_NAMES=("$@")
FILTERED_SCENARIOS="$(json_select_scenarios "${SCENARIOS_JSON}" "${SELECTED_NAMES[@]}")"

ACTIVE_SCENARIOS=()
while IFS= read -r line; do
  [[ -z "${line}" ]] && continue
  ACTIVE_SCENARIOS+=("${line}")
done < <(json_entries "${FILTERED_SCENARIOS}")

if (( ${#ACTIVE_SCENARIOS[@]} == 0 )); then
  echo "!! No matching scenarios found." >&2
  exit 1
fi

failures=()

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
    bootstrap-runner)
      assert_log_contains "${log_file}" "bootstrap env file ok from-dotenv"
      assert_log_contains "${log_file}" "bootstrap env expansion ok from-dotenv-expanded"
      assert_log_contains "${log_file}" "bootstrap job override ok from-job"
      assert_log_contains "${log_file}" "bootstrap mount helper ok"
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
      latest_run=$(json_latest_run_id tests-temp/opal-home/history.json)
      json_verify_preserved_runtime_fields tests-temp/opal-home/opal/history.json
      local summary_path
      summary_path=$(json_preserved_runtime_summary_path tests-temp/opal-home/opal/history.json)
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

scenario_git() {
  env "${GIT_ENV_UNSET[@]}" GIT_TEMPLATE_DIR="${GIT_TEMPLATE_DIR_OPAL}" git "$@"
}

scenario_git_detached() {
  local git_dir="$1"
  local work_tree="$2"
  shift 2
  env "${GIT_ENV_UNSET[@]}" \
    GIT_TEMPLATE_DIR="${GIT_TEMPLATE_DIR_OPAL}" \
    GIT_DIR="${git_dir}" \
    GIT_WORK_TREE="${work_tree}" \
    git "$@"
}

prepare_detached_git_fixture() {
  local workdir="$1"
  local git_tags="$2"
  local git_state_dir="${GIT_STATE_ROOT}/fixture-${TEST_RUN_ID}-$RANDOM"

  mkdir -p "${git_state_dir}"
  scenario_git_detached "${git_state_dir}" "${workdir}" init -b main >/dev/null
  printf 'opal\n' > "${workdir}/README.md"
  scenario_git_detached "${git_state_dir}" "${workdir}" add README.md
  scenario_git_detached "${git_state_dir}" "${workdir}" -c user.name='Opal Tests' -c user.email='opal@example.com' commit -m 'initial' >/dev/null
  local tag
  for tag in ${git_tags}; do
    scenario_git_detached "${git_state_dir}" "${workdir}" tag "${tag}"
  done
  printf '%s\n' "${git_state_dir}" > "${workdir}/.opal-git-dir"
  # Submodule/worktree-style pointer; if denied by sandbox, we still export GIT_DIR/GIT_WORK_TREE at runtime.
  printf 'gitdir: %s\n' "${git_state_dir}" > "${workdir}/.git" 2>/dev/null || true
}

prepare_runtime_git_fixture() {
  local workdir="$1"
  local already_initialized="0"

  mkdir -p "${workdir}"
  mkdir -p "${workdir}/docs"
  : > "${workdir}/Cargo.lock"
  if [[ ! -f "${workdir}/docs/index.md" ]]; then
    printf '# docs\n' > "${workdir}/docs/index.md"
  fi

  if scenario_git -C "${workdir}" rev-parse --git-dir >/dev/null 2>&1; then
    already_initialized="1"
  else
    scenario_git -C "${workdir}" init -b main >/dev/null
  fi

  scenario_git -C "${workdir}" add Cargo.lock docs/index.md >/dev/null 2>&1 || true
  if [[ "${already_initialized}" != "1" ]]; then
    scenario_git -C "${workdir}" -c user.name='Opal Tests' -c user.email='opal@example.com' commit -m 'fixture' >/dev/null 2>&1 || true
  fi
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
        scenario_git -C "${workdir}" init -b main >/dev/null
        printf '# Guide\nbase\n' > "${workdir}/docs/guide.md"
        printf 'fn main() {}\n' > "${workdir}/src/main.rs"
        scenario_git -C "${workdir}" add docs/guide.md src/main.rs
        scenario_git -C "${workdir}" -c user.name='Opal Tests' -c user.email='opal@example.com' commit -m 'base' >/dev/null
        scenario_git -C "${workdir}" checkout -b feature/compare-to >/dev/null
        printf '# Guide\nchanged\n' > "${workdir}/docs/guide.md"
        scenario_git -C "${workdir}" add docs/guide.md
        scenario_git -C "${workdir}" -c user.name='Opal Tests' -c user.email='opal@example.com' commit -m 'docs change' >/dev/null
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
      bootstrap_runner)
        mkdir -p "${workdir}/.opal/bootstrap/scripts"
        cat > "${workdir}/.opal/config.toml" <<'TOML'
[bootstrap]
command = "sh .opal/bootstrap/prepare-runner.sh"
env_file = "bootstrap/generated.env"

[bootstrap.env]
RUNNER_HELPER = "/opal/bootstrap/scripts/helper.sh"
BOOTSTRAP_EXPANDED = "${BOOTSTRAP_BASE}-expanded"

[[bootstrap.mounts]]
host = "bootstrap/scripts"
container = "/opal/bootstrap/scripts"
read_only = true
TOML
        cat > "${workdir}/.opal/bootstrap/prepare-runner.sh" <<'SH'
#!/usr/bin/env sh
set -eu
cat > .opal/bootstrap/generated.env <<'EOF'
BOOTSTRAP_FROM_FILE=from-dotenv
BOOTSTRAP_BASE=from-dotenv
EOF
SH
        chmod +x "${workdir}/.opal/bootstrap/prepare-runner.sh"
        cat > "${workdir}/.opal/bootstrap/scripts/helper.sh" <<'SH'
#!/usr/bin/env sh
set -eu
echo "bootstrap mount helper ok"
SH
        chmod +x "${workdir}/.opal/bootstrap/scripts/helper.sh"
        ;;
      *)
        echo "!! unknown repo setup: ${repo_setup}" >&2
        return 1
        ;;
    esac
    return 0
  fi
  if [[ "${init_git}" == "1" ]]; then
    prepare_detached_git_fixture "${workdir}" "${git_tags}"
  fi
  if [[ -n "${secret_name}" ]]; then
    local secrets_dir="${workdir}/.opal/env"
    mkdir -p "${secrets_dir}"
    printf '%s' "${secret_value}" > "${secrets_dir}/${secret_name}"
  fi
}

prepare_pipeline_for_workdir() {
  local workdir="$1"
  local pipeline_rel="$2"
  local source_path="${REPO_ROOT}/${pipeline_rel}"
  local default_pipeline_path="${workdir}/.gitlab-ci.yml"
  local target_path="${workdir}/${pipeline_rel}"
  local tests_root_rel="pipelines/tests"
  local source_tests_root="${REPO_ROOT}/${tests_root_rel}"
  local target_tests_root="${workdir}/${tests_root_rel}"

  if [[ ! -f "${source_path}" ]]; then
    echo "!! pipeline not found at ${pipeline_rel}" >&2
    return 1
  fi

  if [[ "${pipeline_rel}" == "${tests_root_rel}/"* && -d "${source_tests_root}" ]]; then
    mkdir -p "${target_tests_root}"
    cp -R "${source_tests_root}/." "${target_tests_root}/"
  else
    mkdir -p "$(dirname "${target_path}")"
    cp "${source_path}" "${target_path}"
  fi
  cp "${source_path}" "${default_pipeline_path}"

  # Keep copied pipeline fixtures tracked when the scenario uses a git worktree.
  # This prevents untracked-artifact scenarios from sweeping in fixture files.
  if scenario_git -C "${workdir}" rev-parse --git-dir >/dev/null 2>&1; then
    scenario_git -C "${workdir}" add ".gitlab-ci.yml" "${pipeline_rel}" >/dev/null 2>&1 || true
    if [[ "${pipeline_rel}" == "${tests_root_rel}/"* ]]; then
      scenario_git -C "${workdir}" add "${tests_root_rel}" >/dev/null 2>&1 || true
    fi
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
  local log_name="${name//[^A-Za-z0-9._-]/_}"
  local log_file="${LOG_DIR}/${log_name}.log"
  local workdir="${RUNTIME_WORKDIR_ROOT}/${log_name}-${TEST_RUN_ID}"
  local scenario_git_dir_file=""
  local scenario_git_dir=""
  local scenario_git_dotgit="${workdir}/.git"
  local seed_runtime_git_fixture="0"

  if [[ -n "${workdir_rel}" ]]; then
    if [[ "${workdir_rel}" == /* ]]; then
      workdir="${workdir_rel}"
    else
      workdir="${TMP_RUN_ROOT}/${workdir_rel#./}"
    fi
  elif [[ "${scenario_command:-${OPAL_TEST_COMMAND}}" != "plan" ]]; then
    seed_runtime_git_fixture="1"
  fi
  if [[ "${init_git}" == "1" ]]; then
    workdir="${workdir}-${TEST_RUN_ID}"
    scenario_git_dotgit="${workdir}/.git"
  fi

  prepare_scenario_workdir "${workdir}" "${secret_name}" "${secret_value}" "${init_git}" "${git_tags}" "${repo_setup}"
  if [[ "${seed_runtime_git_fixture}" == "1" && -z "${repo_setup}" && "${init_git}" != "1" ]]; then
    prepare_runtime_git_fixture "${workdir}"
  fi
  if ! prepare_pipeline_for_workdir "${workdir}" "${pipeline_rel}"; then
    echo "!! ${name}: pipeline not found at ${pipeline_rel}" >&2
    return 1
  fi
  scenario_git_dir_file="${workdir}/.opal-git-dir"
  if [[ -f "${scenario_git_dir_file}" ]]; then
    scenario_git_dir="$(<"${scenario_git_dir_file}")"
  fi

  echo "==> ${name}"
  echo "    scenario workdir: ${workdir}"
  echo "    scenario pipeline: ${pipeline_rel}"
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
  local git_fixture_env=()
  if [[ -n "${scenario_git_dir}" ]]; then
    git_fixture_env+=(GIT_DIR="${scenario_git_dir}" GIT_WORK_TREE="${workdir}")
  fi
  local scenario_ci_project_dir="CI_PROJECT_DIR=${workdir}"
  local scenario_xdg_data_home="XDG_DATA_HOME=${XDG_DATA_HOME}"

  if [[ -n "${env_string}" ]]; then
    # shellcheck disable=SC2086
    env "${GIT_ENV_UNSET[@]}" "${SCENARIO_CI_UNSET[@]}" "${git_fixture_env[@]}" "${scenario_ci_project_dir}" "${scenario_xdg_data_home}" ${env_string} "${cmd[@]}" 2>&1 | tee "${log_file}"
  else
    env "${GIT_ENV_UNSET[@]}" "${SCENARIO_CI_UNSET[@]}" "${git_fixture_env[@]}" "${scenario_ci_project_dir}" "${scenario_xdg_data_home}" "${cmd[@]}" 2>&1 | tee "${log_file}"
  fi
  local status=$?
  popd >/dev/null

  if [[ -n "${scenario_git_dir}" ]]; then
    rm -f "${scenario_git_dir_file}" "${scenario_git_dotgit}"
    rm -rf "${scenario_git_dir}"
  fi

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
  local log_file="${LOG_DIR}/cache-fallback.log"
  local namespace="cache-fallback-${TEST_RUN_ID}"
  local common_env="CI_PIPELINE_SOURCE=push CI_DEFAULT_BRANCH=main CI_CACHE_NAMESPACE=${namespace}"
  local cache_root="${XDG_DATA_HOME}/opal/cache"
  local workdir="${RUNTIME_WORKDIR_ROOT}/cache-fallback-${TEST_RUN_ID}"

  : > "${log_file}"
  prepare_scenario_workdir "${workdir}" "" "" "0" "" ""
  prepare_runtime_git_fixture "${workdir}"
  if ! prepare_pipeline_for_workdir "${workdir}" "${pipeline_rel}"; then
    echo "!! cache-fallback: pipeline not found at ${pipeline_rel}" >&2
    return 1
  fi

  local env_string
  local run_index=0
  for env_string in \
    "CI_COMMIT_BRANCH=main ${common_env}" \
    "CI_COMMIT_BRANCH=feature/fallback ${common_env}"
  do
    run_index=$((run_index + 1))
    echo "    scenario workdir: ${workdir}"
    echo "    scenario pipeline: ${pipeline_rel}"
    local cmd=("${OPAL_BIN}" "${OPAL_TEST_COMMAND}")
    if [[ ${#OPAL_ARGS[@]} -gt 0 && -n "${OPAL_ARGS[0]}" ]]; then
      cmd+=("${OPAL_ARGS[@]}")
    fi
    local scenario_ci_project_dir="CI_PROJECT_DIR=${workdir}"
    local scenario_xdg_data_home="XDG_DATA_HOME=${XDG_DATA_HOME}"

    pushd "${workdir}" >/dev/null
    # shellcheck disable=SC2086
    env "${GIT_ENV_UNSET[@]}" "${SCENARIO_CI_UNSET[@]}" "${scenario_ci_project_dir}" "${scenario_xdg_data_home}" ${env_string} "${cmd[@]}" 2>&1 | tee -a "${log_file}"
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
  local log_file="${LOG_DIR}/resource-group-cross-run.log"
  local workdir="${RUNTIME_WORKDIR_ROOT}/resource-group-cross-run-${TEST_RUN_ID}"

  : > "${log_file}"
  prepare_scenario_workdir "${workdir}" "" "" "0" "" ""
  prepare_runtime_git_fixture "${workdir}"
  if ! prepare_pipeline_for_workdir "${workdir}" "${pipeline_rel}"; then
    echo "!! resource-group-cross-run: pipeline not found at ${pipeline_rel}" >&2
    return 1
  fi

  local cmd=("${OPAL_BIN}" "run")
  if [[ ${#OPAL_ARGS[@]} -gt 0 && -n "${OPAL_ARGS[0]}" ]]; then
    cmd+=("${OPAL_ARGS[@]}")
  fi
  local scenario_ci_project_dir="CI_PROJECT_DIR=${workdir}"
  local scenario_xdg_data_home="XDG_DATA_HOME=${XDG_DATA_HOME}"

  echo "    scenario workdir: ${workdir}"
  echo "    scenario pipeline: ${pipeline_rel}"
  pushd "${workdir}" >/dev/null
  local start_ts
  start_ts=$(date +%s)
  env "${GIT_ENV_UNSET[@]}" "${SCENARIO_CI_UNSET[@]}" "${scenario_ci_project_dir}" "${scenario_xdg_data_home}" CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push "${cmd[@]}" > "${log_file}.first" 2>&1 &
  local first_pid=$!
  sleep 1
  env "${GIT_ENV_UNSET[@]}" "${SCENARIO_CI_UNSET[@]}" "${scenario_ci_project_dir}" "${scenario_xdg_data_home}" CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push "${cmd[@]}" > "${log_file}.second" 2>&1
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
  local log_file="${LOG_DIR}/interruptible-abort.log"
  local workdir="${RUNTIME_WORKDIR_ROOT}/interruptible-abort-${TEST_RUN_ID}"

  : > "${log_file}"
  prepare_scenario_workdir "${workdir}" "" "" "0" "" ""
  prepare_runtime_git_fixture "${workdir}"
  if ! prepare_pipeline_for_workdir "${workdir}" "${pipeline_rel}"; then
    echo "!! interruptible-abort: pipeline not found at ${pipeline_rel}" >&2
    return 1
  fi

  local cmd=("${OPAL_BIN}" "run")
  if [[ ${#OPAL_ARGS[@]} -gt 0 && -n "${OPAL_ARGS[0]}" ]]; then
    cmd+=("${OPAL_ARGS[@]}")
  fi
  cmd+=("--max-parallel-jobs" "2")
  local scenario_ci_project_dir="CI_PROJECT_DIR=${workdir}"
  local scenario_xdg_data_home="XDG_DATA_HOME=${XDG_DATA_HOME}"

  echo "    scenario workdir: ${workdir}"
  echo "    scenario pipeline: ${pipeline_rel}"
  pushd "${workdir}" >/dev/null
  # shellcheck disable=SC2086
  env "${GIT_ENV_UNSET[@]}" "${SCENARIO_CI_UNSET[@]}" "${scenario_ci_project_dir}" "${scenario_xdg_data_home}" ${env_string} "${cmd[@]}" > "${log_file}" 2>&1 &
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
  name=$(json_field "${entry}" "name")
  pipeline=$(json_field "${entry}" "pipeline")
  envs=$(json_field "${entry}" "env")
  workdir=$(json_field "${entry}" "workdir" "")
  secret_name=$(json_field "${entry}" "secret_name" "")
  secret_value=$(json_field "${entry}" "secret_value" "")
  init_git=$(json_field "${entry}" "init_git" "0")
  git_tags=$(json_field "${entry}" "git_tags" "")
  repo_setup=$(json_field "${entry}" "repo_setup" "")
  expect_failure=$(json_field "${entry}" "expect_failure" "")
  scenario_command=$(json_field "${entry}" "command" "")
  scenario_opal_args=$(json_field "${entry}" "opal_args" "__DEFAULT__")
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

sync_logs_for_artifacts() {
  mkdir -p "${ARTIFACT_LOG_DIR}"
  shopt -s nullglob
  local files=("${LOG_DIR}"/*.log)
  shopt -u nullglob
  if (( ${#files[@]} == 0 )); then
    return 0
  fi
  cp -f "${files[@]}" "${ARTIFACT_LOG_DIR}/"
}

sync_logs_for_artifacts

if (( ${#failures[@]} > 0 )); then
  echo "!! Test pipeline failures: ${failures[*]}" >&2
  exit 1
fi

echo "✅ All test pipelines completed successfully."
