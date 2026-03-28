#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

die() {
  echo "error: $*" >&2
  exit 1
}

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    die "required environment variable ${name} is not set"
  fi
}

api() {
  local method="$1"
  local url="$2"
  shift 2
  curl -fsSL \
    -X "${method}" \
    -H "Accept: application/vnd.github+json" \
    -H "Authorization: Bearer ${GITHUB_TOKEN}" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    "$@" \
    "${url}"
}

require_env GITHUB_TOKEN
require_env GITHUB_REPOSITORY

TAG="$(bash "${SCRIPT_DIR}/check-release-tag-version.sh" --print-tag)"
VERSION="$(bash "${SCRIPT_DIR}/check-release-tag-version.sh" --print-version)"
OWNER="${GITHUB_REPOSITORY%%/*}"
REPO="${GITHUB_REPOSITORY#*/}"
[[ -n "${OWNER}" && -n "${REPO}" && "${OWNER}" != "${REPO}" ]] || \
  die "GITHUB_REPOSITORY must be in the form owner/repo"

prepare_release_notes() {
  local candidate extension release_notes_path
  mkdir -p releases
  for candidate in \
    "release/notes/${TAG}.md" \
    "release/notes/${VERSION}.md" \
    "release/notes/${TAG}.txt" \
    "release/notes/${VERSION}.txt"; do
    if [[ -f "${candidate}" ]]; then
      extension="${candidate##*.}"
      release_notes_path="releases/release-notes-${VERSION}.${extension}"
      cp "${candidate}" "${release_notes_path}"
      echo "${release_notes_path}"
      return
    fi
  done

  release_notes_path="releases/release-notes-${VERSION}.txt"
  {
    printf 'Release %s\n\n' "${TAG}"
    local found=0
    local note
    while IFS= read -r note; do
      found=1
      cat "${note}"
      printf '\n'
    done < <(find releases -maxdepth 1 -type f -name 'release-notes-*.txt' ! -name "release-notes-${VERSION}.txt" | sort)
    if [[ "${found}" -eq 0 ]]; then
      die "no generated per-platform release notes found and no release/notes/${VERSION}.md or .txt override exists"
    fi
  } >"${release_notes_path}"
  echo "${release_notes_path}"
}

RELEASE_NOTES="$(prepare_release_notes)"

shopt -s nullglob
ASSETS=(releases/*.tar.gz "${RELEASE_NOTES}")
if [[ "${#ASSETS[@]}" -eq 0 ]]; then
  die "no release assets found under releases/"
fi

if [[ "${VERSION}" == *-* ]]; then
  PRERELEASE=true
else
  PRERELEASE=false
fi

RELEASE_JSON="$(mktemp)"
STATUS="$(curl -sS -o "${RELEASE_JSON}" -w '%{http_code}' \
  -H "Accept: application/vnd.github+json" \
  -H "Authorization: Bearer ${GITHUB_TOKEN}" \
  -H "X-GitHub-Api-Version: 2022-11-28" \
  "https://api.github.com/repos/${OWNER}/${REPO}/releases/tags/${TAG}")"

BODY_JSON="$(jq -Rs . < "${RELEASE_NOTES}")"
PAYLOAD="$(jq -n \
  --arg tag_name "${TAG}" \
  --arg name "${TAG}" \
  --argjson body "${BODY_JSON}" \
  --argjson prerelease "${PRERELEASE}" \
  '{tag_name: $tag_name, name: $name, body: $body, draft: false, prerelease: $prerelease, generate_release_notes: false}')"

case "${STATUS}" in
  200)
    RELEASE_ID="$(jq -r '.id' < "${RELEASE_JSON}")"
    api PATCH "https://api.github.com/repos/${OWNER}/${REPO}/releases/${RELEASE_ID}" \
      --data "${PAYLOAD}" >/tmp/opal-github-release.json
    ;;
  404)
    api POST "https://api.github.com/repos/${OWNER}/${REPO}/releases" \
      --data "${PAYLOAD}" >/tmp/opal-github-release.json
    ;;
  *)
    cat "${RELEASE_JSON}" >&2
    die "failed to fetch GitHub release for tag ${TAG} (status ${STATUS})"
    ;;
esac

RELEASE_ID="$(jq -r '.id' < /tmp/opal-github-release.json)"
UPLOAD_URL="$(jq -r '.upload_url' < /tmp/opal-github-release.json | sed 's/{?name,label}$//')"
ASSETS_JSON="$(api GET "https://api.github.com/repos/${OWNER}/${REPO}/releases/${RELEASE_ID}/assets")"

for asset in "${ASSETS[@]}"; do
  name="$(basename "${asset}")"
  existing_id="$(jq -r --arg name "${name}" '.[] | select(.name == $name) | .id' <<<"${ASSETS_JSON}" | head -n1)"
  if [[ -n "${existing_id}" ]]; then
    api DELETE "https://api.github.com/repos/${OWNER}/${REPO}/releases/assets/${existing_id}" >/dev/null
  fi
  curl -fsSL \
    -X POST \
    -H "Accept: application/vnd.github+json" \
    -H "Authorization: Bearer ${GITHUB_TOKEN}" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    -H "Content-Type: application/octet-stream" \
    "${UPLOAD_URL}?name=${name}" \
    --data-binary "@${asset}" >/dev/null
done

echo "published GitHub release ${TAG} to ${GITHUB_REPOSITORY}"
