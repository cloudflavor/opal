#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

HOST_OS="$(uname -s)"
HOST_ARCH="$(uname -m)"

RUST_IMAGE="${RUST_IMAGE:-docker.io/library/rust:1.90}"
CONTAINER_CPUS="${CONTAINER_CPUS:-4}"
CONTAINER_MEMORY="${CONTAINER_MEMORY:-2g}"
CONTAINER_DNS="${CONTAINER_DNS:-1.1.1.1}"
HOST_CARGO_HOME="${HOST_CARGO_HOME:-${REPO_ROOT}/target/.container-cache/cargo-home}"
HOST_RUSTUP_HOME="${HOST_RUSTUP_HOME:-${REPO_ROOT}/target/.container-cache/rustup-home}"
TARGET_DIR_SLUG="${CI_JOB_NAME_SLUG:-release-artifacts}"
if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  if [[ "${HOST_OS}" == "Darwin" ]]; then
    CARGO_TARGET_DIR="${CONTAINER_HOST_TARGET_DIR:-${TMPDIR:-/tmp}/opal-release-target/${TARGET_DIR_SLUG}}"
  else
    CARGO_TARGET_DIR="${REPO_ROOT}/target/${TARGET_DIR_SLUG}"
  fi
fi
mkdir -p "${HOST_CARGO_HOME}" "${HOST_RUSTUP_HOME}" "${CARGO_TARGET_DIR}" "${REPO_ROOT}/target" "${REPO_ROOT}/releases"
HOST_CARGO_HOME="$(cd "${HOST_CARGO_HOME}" && pwd)"
HOST_RUSTUP_HOME="$(cd "${HOST_RUSTUP_HOME}" && pwd)"
export CARGO_TARGET_DIR

TARGET_MATRIX=(
  "aarch64-apple-darwin:local:aarch64-apple-silicon"
  "aarch64-unknown-linux-gnu:linux:aarch64-unknown-linux-gnu"
  "x86_64-unknown-linux-gnu:linux:x86_64-unknown-linux-gnu"
)

target_filter_matches() {
  local target="$1"
  local label="$2"
  local filter="$3"

  if [[ "${filter}" == "${target}" || "${filter}" == "${label}" ]]; then
    return 0
  fi

  case "${target}" in
    x86_64-unknown-linux-gnu)
      case "${filter}" in
        amd64|linux-amd64|linux/amd64|x86_64-linux)
          return 0
          ;;
      esac
      ;;
    aarch64-unknown-linux-gnu)
      case "${filter}" in
        arm64|linux-arm64|linux-aarch64|linux/arm64|aarch64-linux)
          return 0
          ;;
      esac
      ;;
    aarch64-apple-darwin)
      case "${filter}" in
        macos-aarch64|macos-arm64|macos/arm64|darwin-arm64|darwin-aarch64|darwin/arm64)
          return 0
          ;;
      esac
      ;;
  esac

  return 1
}

target_selected() {
  local target="$1"
  local label="$2"
  if [[ -z "${RELEASE_TARGETS:-}" ]]; then
    return 0
  fi

  local filter
  IFS=',' read -r -a filters <<<"${RELEASE_TARGETS}"
  for filter in "${filters[@]}"; do
    filter="${filter//[[:space:]]/}"
    if [[ -z "${filter}" ]]; then
      continue
    fi
    if target_filter_matches "${target}" "${label}" "${filter}"; then
      return 0
    fi
  done
  return 1
}

log() {
  printf '==> %s\n' "$*"
}

die() {
  echo "error: $*" >&2
  exit 1
}

detect_container_cli() {
  if [[ -n "${CONTAINER_CLI:-}" ]]; then
    return
  fi
  local candidates=(container docker podman nerdctl)
  for candidate in "${candidates[@]}"; do
    if command -v "${candidate}" >/dev/null 2>&1; then
      CONTAINER_CLI="${candidate}"
      return
    fi
  done
}

ensure_container_helper() {
  if [[ -n "${CONTAINER_HELPER:-}" && -f "${CONTAINER_HELPER}" ]]; then
    return
  fi
  CONTAINER_HELPER="${REPO_ROOT}/target/.release-container-build.sh"
  cat >"${CONTAINER_HELPER}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

TARGET_TRIPLE="${TARGET_TRIPLE:?missing target triple}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/work/target}"

ensure_toolchain() {
  rustup target add "${TARGET_TRIPLE}" >/dev/null
  if [[ "${TARGET_TRIPLE}" == "aarch64-unknown-linux-gnu" ]]; then
    if ! command -v aarch64-linux-gnu-gcc >/dev/null 2>&1; then
      if command -v apt-get >/dev/null 2>&1; then
        apt-get update
        DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu
      else
        echo "missing aarch64-linux-gnu toolchain and apt-get is unavailable" >&2
        exit 1
      fi
    fi
  fi
}

ensure_toolchain
cargo build -p opal-cli --bin opal --release --locked --target "${TARGET_TRIPLE}"
EOF
  chmod +x "${CONTAINER_HELPER}"
  CONTAINER_HELPER_CONTAINER="/work/${CONTAINER_HELPER#"${REPO_ROOT}/"}"
}

cleanup() {
  if [[ -n "${CONTAINER_HELPER:-}" && -f "${CONTAINER_HELPER}" ]]; then
    rm -f "${CONTAINER_HELPER}"
  fi
}
trap cleanup EXIT

maybe_use_sudo() {
  if [[ "${EUID}" -eq 0 ]]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    die "command '$*' requires root privileges; install sudo or run as root"
  fi
}

APT_UPDATED=0
ensure_package() {
  local package="$1"
  if command -v dpkg >/dev/null 2>&1 && ! dpkg -s "${package}" >/dev/null 2>&1; then
    if [[ "${APT_UPDATED}" -eq 0 ]]; then
      log "Updating apt cache to install ${package}"
      maybe_use_sudo apt-get update
      APT_UPDATED=1
    fi
    log "Installing ${package}"
    maybe_use_sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${package}"
  fi
}

run_container_build() {
  local target="$1"
  detect_container_cli
  if [[ -z "${CONTAINER_CLI:-}" ]]; then
    die "no container CLI found (install Apple's container CLI, Docker, Podman, or NerdCTL)"
  fi

  ensure_container_helper
  local helper_in_container="${CONTAINER_HELPER_CONTAINER}"
  local -a dns_args=()
  if [[ -n "${CONTAINER_DNS}" ]]; then
    dns_args=(--dns "${CONTAINER_DNS}")
  fi

  case "${CONTAINER_CLI}" in
    container)
      local name="opal-release-${target//[^a-zA-Z0-9]/-}-$$"
      local status=0
      if ! "${CONTAINER_CLI}" run \
        --arch x86_64 \
        --name "${name}" \
        --workdir /work \
        --cpus "${CONTAINER_CPUS}" \
        --memory "${CONTAINER_MEMORY}" \
        "${dns_args[@]}" \
        --env "CARGO_HOME=/cargo-home" \
        --env "RUSTUP_HOME=/rustup-home" \
        --env "CARGO_TARGET_DIR=/target" \
        --env "CARGO_TARGET_DIR=/target" \
        --env "TARGET_TRIPLE=${target}" \
        --volume "${REPO_ROOT}:/work" \
        --volume "${HOST_CARGO_HOME}:/cargo-home" \
        --volume "${HOST_RUSTUP_HOME}:/rustup-home" \
        --volume "${CARGO_TARGET_DIR}:/target" \
        "${RUST_IMAGE}" \
        bash "${helper_in_container}"; then
        status=$?
      fi
      "${CONTAINER_CLI}" rm "${name}" >/dev/null 2>&1 || true
      return "${status}"
      ;;
    docker|podman)
      local remove_flag="--rm"
      local platform_args=()
      if [[ "${CONTAINER_CLI}" != "podman" ]]; then
        platform_args=(--platform linux/amd64)
      fi
      "${CONTAINER_CLI}" run \
        ${remove_flag} \
        "${platform_args[@]}" \
        "${dns_args[@]}" \
        -w /work \
        -e "CARGO_HOME=/cargo-home" \
        -e "RUSTUP_HOME=/rustup-home" \
        -e "CARGO_TARGET_DIR=/target" \
        -e "TARGET_TRIPLE=${target}" \
        -v "${REPO_ROOT}:/work" \
        -v "${HOST_CARGO_HOME}:/cargo-home" \
        -v "${HOST_RUSTUP_HOME}:/rustup-home" \
        -v "${CARGO_TARGET_DIR}:/target" \
        "${RUST_IMAGE}" \
        bash "${helper_in_container}"
      ;;
    nerdctl)
      "${CONTAINER_CLI}" run \
        --rm \
        "${dns_args[@]}" \
        -w /work \
        -e "CARGO_HOME=/cargo-home" \
        -e "RUSTUP_HOME=/rustup-home" \
        -e "CARGO_TARGET_DIR=/target" \
        -e "TARGET_TRIPLE=${target}" \
        -v "${REPO_ROOT}:/work" \
        -v "${HOST_CARGO_HOME}:/cargo-home" \
        -v "${HOST_RUSTUP_HOME}:/rustup-home" \
        -v "${CARGO_TARGET_DIR}:/target" \
        "${RUST_IMAGE}" \
        bash "${helper_in_container}"
      ;;
    *)
      die "unsupported container CLI '${CONTAINER_CLI}'"
      ;;
  esac
}

build_local_target() {
  local target="$1"
  rustup target add "${target}" >/dev/null
  cargo build -p opal-cli --bin opal --release --locked --target "${target}"
}

build_linux_target() {
  local target="$1"
  if [[ "${HOST_OS}" == "Darwin" ]]; then
    detect_container_cli
    log "Building ${target} inside ${CONTAINER_CLI:-container} container"
    run_container_build "${target}"
  else
    if [[ "${target}" == "aarch64-unknown-linux-gnu" ]]; then
      ensure_package gcc-aarch64-linux-gnu
    elif [[ "${target}" == "x86_64-unknown-linux-gnu" && "${HOST_ARCH}" != "x86_64" ]]; then
      ensure_package gcc-x86-64-linux-gnu
    fi
    build_local_target "${target}"
  fi
}

package_artifact() {
  local target="$1"
  local platform_label="$2"
  local binary_dir="${CARGO_TARGET_DIR}/${target}/release"
  local binary_path="${binary_dir}/opal"
  if [[ ! -f "${binary_path}" ]]; then
    die "expected binary ${binary_path} not found"
  fi
  local archive="${RELEASE_DIR}/opal-${VERSION}-${platform_label}.tar.gz"
  tar -czf "${archive}" -C "${binary_dir}" opal

  local checksum
  if command -v shasum >/dev/null 2>&1; then
    checksum="$(shasum -a 256 "${archive}" | awk '{print $1}')"
  elif command -v sha256sum >/dev/null 2>&1; then
    checksum="$(sha256sum "${archive}" | awk '{print $1}')"
  else
    die "neither shasum nor sha256sum is available to compute checksums"
  fi
  cat >"${RELEASE_DIR}/release-notes-${platform_label}.txt" <<EOF
Release archive: $(basename "${archive}")
SHA-256: ${checksum}
Target: ${platform_label}
EOF
  log "Wrote ${archive} (${platform_label})"
}

VERSION="$(bash "${SCRIPT_DIR}/check-release-tag-version.sh" --print-version)"
log "Building release artifacts for ${VERSION}"

RELEASE_DIR="${REPO_ROOT}/releases"

BUILT_TARGETS=()

for entry in "${TARGET_MATRIX[@]}"; do
  IFS=":" read -r target mode label <<<"${entry}"
  if ! target_selected "${target}" "${label}"; then
    continue
  fi
  case "${mode}" in
    local)
      if [[ "${HOST_OS}" != "Darwin" ]]; then
        log "Skipping ${label} (requires macOS host)"
        continue
      fi
      log "Building ${label} locally (${target})"
      build_local_target "${target}"
      ;;
    linux)
      log "Building ${label} (${target})"
      build_linux_target "${target}"
      ;;
    *)
      die "unknown build mode '${mode}'"
      ;;
  esac
  package_artifact "${target}" "${label}"
  BUILT_TARGETS+=("${label}")
done

if [[ "${#BUILT_TARGETS[@]}" -eq 0 ]]; then
  die "no artifacts were produced; check host requirements"
fi

log "Artifacts ready in ${RELEASE_DIR}: ${BUILT_TARGETS[*]}"
