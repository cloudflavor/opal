#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_REPO_ROOT="${CI_PROJECT_DIR:-$(cd "${SCRIPT_DIR}/.." && pwd)}"

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <extended-tests|e2e-tests> [scenario ...]" >&2
  exit 2
fi

SUITE_NAME="$1"
shift

case "${SUITE_NAME}" in
  extended-tests|e2e-tests)
    ;;
  *)
    echo "unsupported suite '${SUITE_NAME}'; expected extended-tests or e2e-tests" >&2
    exit 2
    ;;
esac

export PATH="/usr/local/bin:/opt/homebrew/bin:${PATH}"

TMP_PARENT="${OPAL_TMP_TEST_ROOT:-/tmp}"
mkdir -p "${TMP_PARENT}"
TMP_RUN_ROOT="$(mktemp -d "${TMP_PARENT%/}/opal-${SUITE_NAME}-XXXXXX")"
TMP_REPO_ROOT="${TMP_RUN_ROOT}/repo"
mkdir -p "${TMP_REPO_ROOT}"

export XDG_DATA_HOME="${TMP_RUN_ROOT}/xdg-data"
mkdir -p "${XDG_DATA_HOME}"

cleanup_tmp_root() {
  if [[ "${OPAL_KEEP_TMP_TEST_ROOT:-0}" == "1" ]]; then
    echo "keeping temp suite root: ${TMP_RUN_ROOT}"
    return
  fi
  rm -rf "${TMP_RUN_ROOT}"
}
trap cleanup_tmp_root EXIT

echo "using temp suite repo: ${TMP_REPO_ROOT}"

(
  cd "${SOURCE_REPO_ROOT}"
  tar \
    --exclude='./.git' \
    --exclude='./target' \
    --exclude='./tests-temp' \
    -cf - .
) | (
  cd "${TMP_REPO_ROOT}"
  tar -xf -
)

remap_to_tmp_repo() {
  local value="$1"
  if [[ "${value}" != /* ]]; then
    printf '%s\n' "${TMP_REPO_ROOT}/${value#./}"
    return
  fi
  if [[ "${value}" == "${SOURCE_REPO_ROOT}"* ]]; then
    printf '%s\n' "${TMP_REPO_ROOT}${value#"${SOURCE_REPO_ROOT}"}"
    return
  fi
  printf '%s\n' "${value}"
}

if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  CARGO_TARGET_DIR="$(remap_to_tmp_repo "${CARGO_TARGET_DIR}")"
else
  CARGO_TARGET_DIR="${TMP_REPO_ROOT}/target/${CI_JOB_NAME_SLUG:-job}"
fi
export CARGO_TARGET_DIR
mkdir -p "${CARGO_TARGET_DIR}"
export RUSTC_WRAPPER=""
export SCCACHE_DISABLE="1"

if [[ -n "${CARGO_HOME:-}" ]]; then
  if [[ "${CARGO_HOME}" != /* ]]; then
    CARGO_HOME="$(remap_to_tmp_repo "${CARGO_HOME}")"
    export CARGO_HOME
  fi
fi

pushd "${TMP_REPO_ROOT}" >/dev/null

if [[ "${SUITE_NAME}" == "e2e-tests" ]]; then
  cargo test --workspace --tests --locked
fi

cargo build -p opal-cli --bin opal --locked

export OPAL_BIN="${CARGO_TARGET_DIR}/debug/opal"
export OPAL_TEST_SUITE="${SUITE_NAME}"
export OPAL_TEST_ARGS="${OPAL_TEST_ARGS:---no-tui}"

./scripts/test-pipelines.sh "$@"

popd >/dev/null
