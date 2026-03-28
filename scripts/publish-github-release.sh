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

api_capture() {
  local method="$1"
  local url="$2"
  shift 2
  local body_file status
  body_file="$(mktemp)"
  status="$(curl -sS -o "${body_file}" -w '%{http_code}' \
    -X "${method}" \
    -H "Accept: application/vnd.github+json" \
    -H "Authorization: Bearer ${GITHUB_TOKEN}" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    "$@" \
    "${url}")"
  printf '%s\n%s\n' "${status}" "${body_file}"
}

api_expect_success() {
  local step="$1"
  local method="$2"
  local url="$3"
  shift 3
  local captured status body_file
  captured="$(api_capture "${method}" "${url}" "$@")"
  status="$(printf '%s\n' "${captured}" | sed -n '1p')"
  body_file="$(printf '%s\n' "${captured}" | sed -n '2p')"
  if [[ "${status}" =~ ^2 ]]; then
    cat "${body_file}"
    rm -f "${body_file}"
    return 0
  fi

  echo "error: ${step} failed (status ${status})" >&2
  cat "${body_file}" >&2 || true
  rm -f "${body_file}"
  exit 1
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
  local candidate extension release_notes_path generated_dir
  generated_dir="${TMPDIR:-/tmp}/opal-release-notes"
  mkdir -p "${generated_dir}"
  for candidate in \
    "release/notes/${TAG}.md" \
    "release/notes/${VERSION}.md" \
    "release/notes/${TAG}.txt" \
    "release/notes/${VERSION}.txt"; do
    if [[ -f "${candidate}" ]]; then
      extension="${candidate##*.}"
      release_notes_path="${generated_dir}/release-notes-${VERSION}.${extension}"
      cp "${candidate}" "${release_notes_path}"
      echo "${release_notes_path}"
      return
    fi
  done

  release_notes_path="${generated_dir}/release-notes-${VERSION}.txt"
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
    api_expect_success \
      "update GitHub release ${TAG}" \
      PATCH \
      "https://api.github.com/repos/${OWNER}/${REPO}/releases/${RELEASE_ID}" \
      --data "${PAYLOAD}" >/tmp/opal-github-release.json
    ;;
  404)
    api_expect_success \
      "create GitHub release ${TAG}" \
      POST \
      "https://api.github.com/repos/${OWNER}/${REPO}/releases" \
      --data "${PAYLOAD}" >/tmp/opal-github-release.json
    ;;
  *)
    cat "${RELEASE_JSON}" >&2
    die "failed to fetch GitHub release for tag ${TAG} (status ${STATUS})"
    ;;
esac

RELEASE_ID="$(jq -r '.id' < /tmp/opal-github-release.json)"
RELEASE_HTML_URL="$(jq -r '.html_url' < /tmp/opal-github-release.json)"
UPLOAD_URL="$(jq -r '.upload_url' < /tmp/opal-github-release.json | sed 's/{?name,label}$//')"
ASSETS_JSON="$(api_expect_success \
  "list assets for GitHub release ${TAG}" \
  GET \
  "https://api.github.com/repos/${OWNER}/${REPO}/releases/${RELEASE_ID}/assets")"

UPLOADED_URLS=()
for asset in "${ASSETS[@]}"; do
  name="$(basename "${asset}")"
  existing_id="$(jq -r --arg name "${name}" '.[] | select(.name == $name) | .id' <<<"${ASSETS_JSON}" | head -n1)"
  if [[ -n "${existing_id}" ]]; then
    api_expect_success \
      "delete existing GitHub asset ${name}" \
      DELETE \
      "https://api.github.com/repos/${OWNER}/${REPO}/releases/assets/${existing_id}" >/dev/null
  fi
  upload_json="$(api_expect_success \
    "upload GitHub asset ${name}" \
    POST \
    "${UPLOAD_URL}?name=${name}" \
    -H "Content-Type: application/octet-stream" \
    --data-binary "@${asset}")"
  uploaded_url="$(jq -r '.browser_download_url // empty' <<<"${upload_json}")"
  if [[ -n "${uploaded_url}" ]]; then
    UPLOADED_URLS+=("${uploaded_url}")
  fi
done

echo "published GitHub release ${TAG}"
echo "release url: ${RELEASE_HTML_URL}"
if [[ "${#UPLOADED_URLS[@]}" -gt 0 ]]; then
  echo "uploaded assets:"
  for url in "${UPLOADED_URLS[@]}"; do
    echo "  - ${url}"
  done
fi
