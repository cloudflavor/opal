#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

die() {
  echo "error: $*" >&2
  exit 1
}

if [[ "$(uname -s)" != "Darwin" ]]; then
  die "this helper is intended to run on a macOS host"
fi

VERSION="$(bash "${SCRIPT_DIR}/check-release-tag-version.sh" --print-version)"

RELEASE_TARGETS="aarch64-apple-silicon" bash "${SCRIPT_DIR}/build-release-artifacts.sh"

ARCHIVE="${REPO_ROOT}/releases/opal-${VERSION}-aarch64-apple-silicon.tar.gz"
[[ -f "${ARCHIVE}" ]] || die "expected artifact ${ARCHIVE} not found"

if command -v shasum >/dev/null 2>&1; then
  CHECKSUM="$(shasum -a 256 "${ARCHIVE}" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
  CHECKSUM="$(sha256sum "${ARCHIVE}" | awk '{print $1}')"
else
  die "neither shasum nor sha256sum is available to compute checksums"
fi

echo "artifact: ${ARCHIVE}"
echo "sha256: ${CHECKSUM}"
