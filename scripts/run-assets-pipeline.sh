#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_ROOT="$ROOT_DIR/.opal-installed"
INSTALL_BIN_DIR="$INSTALL_ROOT/bin"

cd "$ROOT_DIR"

export PATH="$INSTALL_BIN_DIR:$PATH"

cargo install --path crates/opal --locked --root "$INSTALL_ROOT"

"$INSTALL_BIN_DIR/opal" run \
  --pipeline "$ROOT_DIR/.gitlab-ci.yml" \
  --workdir "$ROOT_DIR" \
  --base-image "docker.io/library/rust:1.90"
