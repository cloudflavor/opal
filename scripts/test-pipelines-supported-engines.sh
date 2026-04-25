#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

ENGINES=(docker podman container orbstack)
BASE_ARGS="${OPAL_TEST_ARGS:-"--no-tui --max-parallel-jobs 1"}"
failures=()

engine_available() {
  case "$1" in
    docker|orbstack)
      docker info >/dev/null 2>&1
      ;;
    podman)
      podman info >/dev/null 2>&1
      ;;
    container)
      container system status >/dev/null 2>&1 || command -v container >/dev/null 2>&1
      ;;
    *)
      return 1
      ;;
  esac
}

for engine in "${ENGINES[@]}"; do
  echo "==> engine: ${engine}"
  if ! engine_available "${engine}"; then
    echo "!! engine unavailable: ${engine}" >&2
    failures+=("${engine}: unavailable")
    continue
  fi

  if ! OPAL_TEST_ARGS="${BASE_ARGS} --engine ${engine}" "${REPO_ROOT}/scripts/test-pipelines.sh" "$@"; then
    failures+=("${engine}")
  fi
done

if (( ${#failures[@]} > 0 )); then
  echo "!! Supported-engine test failures: ${failures[*]}" >&2
  exit 1
fi

echo "✅ Supported-engine matrix completed successfully."
