#!/usr/bin/env bash
#
# Bootstrap a fresh macOS install into a "Rust-ready" CI worker.
# Usage: sudo scripts/bootstrap-macos-rust.sh

set -euo pipefail

abort() {
  echo "error: $*" >&2
  exit 1
}

require_root() {
  if [[ ${EUID} -ne 0 ]]; then
    abort "run this script with sudo/root privileges"
  fi
}

ensure_command_line_tools() {
  if xcode-select -p >/dev/null 2>&1; then
    echo "✓ Command Line Tools already installed"
    return
  fi

  echo "→ Installing Command Line Tools for Xcode (headless)…"
  touch /tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress
  CLT_LABEL="$(/usr/sbin/softwareupdate -l | awk -F'*' '/\* Label: Command Line Tools/ {print $2}' | sed -e 's/^ *Label: //' -e 's/^ *//;q')"
  rm -f /tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress

  [[ -n "${CLT_LABEL}" ]] || abort "failed to locate Command Line Tools label via softwareupdate"
  /usr/sbin/softwareupdate --install "${CLT_LABEL}" --verbose
  /usr/bin/xcode-select --switch /Library/Developer/CommandLineTools
  echo "✓ Command Line Tools installed"
}

install_rosetta_if_needed() {
  if [[ "$(uname -m)" != "arm64" ]]; then
    return
  fi

  if /usr/bin/pgrep oahd >/dev/null 2>&1 || /usr/sbin/pkgutil --pkg-info=com.apple.pkg.RosettaUpdateAuto >/dev/null 2>&1; then
    echo "✓ Rosetta already available"
    return
  fi

  echo "→ Installing Rosetta 2 (for x86-only crates)…"
  /usr/sbin/softwareupdate --install-rosetta --agree-to-license
}

install_rustup() {
  local rustup_home="/opt/rustup"
  local cargo_home="/opt/cargo"
  local target_user="${SUDO_USER:-root}"
  local target_group
  target_group="$(id -gn "${target_user}")"

  if [[ -x "${cargo_home}/bin/rustc" ]]; then
    echo "✓ Rust toolchain already present at ${cargo_home}"
    return
  fi

  echo "→ Installing rustup (minimal profile)…"
  install -d -m 0755 "${rustup_home}" "${cargo_home}"
  chown -R "${target_user}:${target_group}" "${rustup_home}" "${cargo_home}"

  sudo -u "${target_user}" \
    env RUSTUP_HOME="${rustup_home}" CARGO_HOME="${cargo_home}" \
    /usr/bin/curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
    sudo -u "${target_user}" env RUSTUP_HOME="${rustup_home}" CARGO_HOME="${cargo_home}" sh -s -- -y --profile minimal

  sudo -u "${target_user}" \
    env RUSTUP_HOME="${rustup_home}" CARGO_HOME="${cargo_home}" \
    "${cargo_home}/bin/rustup" component add rustfmt clippy

  install -d -m 0755 /etc/profile.d
  {
    echo "export RUSTUP_HOME=${rustup_home}"
    echo "export CARGO_HOME=${cargo_home}"
    echo "export PATH=\$CARGO_HOME/bin:\$PATH"
  } >/etc/profile.d/rust.sh
  chmod 0644 /etc/profile.d/rust.sh

  echo "✓ Rust toolchain installed to ${cargo_home}"
}

main() {
  require_root
  ensure_command_line_tools
  install_rosetta_if_needed
  install_rustup

  echo
  echo "macOS VM is ready for Rust builds."
  echo "Remember to source /etc/profile.d/rust.sh in non-login shells."
}

main "$@"
