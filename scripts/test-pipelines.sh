#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OPAL_BIN="${OPAL_BIN:-target/release/opal}"
OPAL_TEST_COMMAND="${OPAL_TEST_COMMAND:-run}"
DEFAULT_ARGS="--no-tui --max-parallel-jobs 1"
read -r -a OPAL_ARGS <<<"${OPAL_TEST_ARGS:-$DEFAULT_ARGS}"
LOG_DIR="${REPO_ROOT}/tests-temp/test-pipeline-logs"
mkdir -p "${LOG_DIR}"

if [[ "${OPAL_BIN}" != /* ]]; then
  OPAL_BIN="${REPO_ROOT}/${OPAL_BIN}"
fi

SCENARIOS_JSON='[
  {"name":"needs-branch","pipeline":"pipelines/tests/needs-and-artifacts.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"needs-optional","pipeline":"pipelines/tests/needs-and-artifacts.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push ENABLE_OPTIONAL=1"},
  {"name":"needs-tag","pipeline":"pipelines/tests/needs-and-artifacts.gitlab-ci.yml","env":"CI_COMMIT_TAG=v1.2.3 CI_PIPELINE_SOURCE=push"},
  {"name":"rules-schedule","pipeline":"pipelines/tests/rules-playground.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=schedule RUN_DELAYED=1"},
  {"name":"rules-force-docs","pipeline":"pipelines/tests/rules-playground.gitlab-ci.yml","env":"CI_PIPELINE_SOURCE=push FORCE_DOCS=1"},
  {"name":"includes-inherit","pipeline":"pipelines/tests/includes-and-extends.gitlab-ci.yml","env":"SKIP_INHERIT=1"},
  {"name":"resources-services","pipeline":"pipelines/tests/resources-and-services.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"},
  {"name":"filters-branch","pipeline":"pipelines/tests/filters.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=feature/foo CI_PIPELINE_SOURCE=push"},
  {"name":"filters-tag","pipeline":"pipelines/tests/filters.gitlab-ci.yml","env":"CI_COMMIT_TAG=v1.2.0 CI_PIPELINE_SOURCE=push"},
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

mapfile -t ACTIVE_SCENARIOS < <(jq -c '.[]' <<<"${FILTERED_SCENARIOS}")

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
  esac
}

prepare_scenario_workdir() {
  local workdir="$1"
  local secret_name="$2"
  local secret_value="$3"

  mkdir -p "${workdir}"
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
  local pipeline_path="${REPO_ROOT}/${pipeline_rel}"
  local log_name="${name//[^A-Za-z0-9._-]/_}"
  local log_file="${LOG_DIR}/${log_name}.log"
  local workdir="${REPO_ROOT}"

  if [[ -n "${workdir_rel}" ]]; then
    workdir="${REPO_ROOT}/${workdir_rel}"
  fi

  if [[ ! -f "${pipeline_path}" ]]; then
    echo "!! ${name}: pipeline not found at ${pipeline_rel}" >&2
    return 1
  fi

  prepare_scenario_workdir "${workdir}" "${secret_name}" "${secret_value}"

  echo "==> ${name}"
  pushd "${workdir}" >/dev/null

  local cmd=("${OPAL_BIN}" "${OPAL_TEST_COMMAND}")
  if [[ ${#OPAL_ARGS[@]} -gt 0 && -n "${OPAL_ARGS[0]}" ]]; then
    cmd+=("${OPAL_ARGS[@]}")
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

for entry in "${ACTIVE_SCENARIOS[@]}"; do
  name=$(jq -r '.name' <<<"${entry}")
  pipeline=$(jq -r '.pipeline' <<<"${entry}")
  envs=$(jq -r '.env' <<<"${entry}")
  workdir=$(jq -r '.workdir // ""' <<<"${entry}")
  secret_name=$(jq -r '.secret_name // ""' <<<"${entry}")
  secret_value=$(jq -r '.secret_value // ""' <<<"${entry}")
  if ! run_scenario "${name}" "${pipeline}" "${envs}" "${workdir}" "${secret_name}" "${secret_value}"; then
    failures+=("${name}")
  fi
done

if (( ${#failures[@]} > 0 )); then
  echo "!! Test pipeline failures: ${failures[*]}" >&2
  exit 1
fi

echo "✅ All test pipelines completed successfully."
