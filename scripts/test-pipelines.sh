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
  {"name":"environment-stop","pipeline":"pipelines/tests/environments.gitlab-ci.yml","env":"CI_COMMIT_BRANCH=main CI_PIPELINE_SOURCE=push"}
]'

SELECTED_NAMES=("$@")
if (( ${#SELECTED_NAMES[@]} > 0 )); then
  names_json=$(printf '%s\n' "${SELECTED_NAMES[@]}" | jq -R . | jq -s .)
  FILTERED_SCENARIOS=$(jq --argjson names "${names_json}" '[ .[] | select($names | index(.name)) ]' <<<"${SCENARIOS_JSON}")
else
  FILTERED_SCENARIOS="${SCENARIOS_JSON}"
fi

mapfile -t ACTIVE_SCENARIOS < <(jq -c '.[]' <<<"${FILTERED_SCENARIOS}")

if (( ${#ACTIVE_SCENARIOS[@]} == 0 )); then
  echo "!! No matching scenarios found." >&2
  exit 1
fi

failures=()

run_scenario() {
  local name="$1"
  local pipeline_rel="$2"
  local env_string="$3"
  local pipeline_path="${REPO_ROOT}/${pipeline_rel}"
  local log_name="${name//[^A-Za-z0-9._-]/_}"
  local log_file="${LOG_DIR}/${log_name}.log"

  if [[ ! -f "${pipeline_path}" ]]; then
    echo "!! ${name}: pipeline not found at ${pipeline_rel}" >&2
    return 1
  fi

  echo "==> ${name}"
  pushd "${REPO_ROOT}" >/dev/null

  local cmd=("${OPAL_BIN}" "${OPAL_TEST_COMMAND}")
  if [[ ${#OPAL_ARGS[@]} -gt 0 && -n "${OPAL_ARGS[0]}" ]]; then
    cmd+=("${OPAL_ARGS[@]}")
  fi
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
  if ! run_scenario "${name}" "${pipeline}" "${envs}"; then
    failures+=("${name}")
  fi
done

if (( ${#failures[@]} > 0 )); then
  echo "!! Test pipeline failures: ${failures[*]}" >&2
  exit 1
fi

echo "✅ All test pipelines completed successfully."
