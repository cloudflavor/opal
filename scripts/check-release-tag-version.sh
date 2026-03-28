#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

die() {
  echo "error: $*" >&2
  exit 1
}

release_tag() {
  local tag="${CI_COMMIT_TAG:-}"
  if [[ -z "${tag}" ]]; then
    tag="$(git -C "${REPO_ROOT}" describe --tags --exact-match HEAD 2>/dev/null || true)"
  fi
  if [[ -z "${tag}" ]]; then
    die "release publishing requires CI_COMMIT_TAG or an exact tag on HEAD"
  fi
  echo "${tag}"
}

manifest_version() {
  local version
  version="$(awk -F '"' '
    $0 == "[package]" { in_package = 1; next }
    /^\[/ && $0 != "[package]" { in_package = 0 }
    in_package && $1 ~ /^version = / { print $2; exit }
  ' "${REPO_ROOT}/Cargo.toml")"
  if [[ -z "${version}" ]]; then
    die "failed to read [package].version from Cargo.toml"
  fi
  echo "${version}"
}

assert_match() {
  local tag version normalized_tag
  tag="$(release_tag)"
  version="$(manifest_version)"
  normalized_tag="${tag#v}"
  if [[ "${normalized_tag}" != "${version}" ]]; then
    die "tag '${tag}' does not match Cargo.toml version '${version}'"
  fi
}

case "${1:-check}" in
  --print-tag)
    assert_match
    release_tag
    ;;
  --print-version)
    assert_match
    manifest_version
    ;;
  check)
    assert_match
    ;;
  *)
    die "usage: $0 [check|--print-tag|--print-version]"
    ;;
esac
