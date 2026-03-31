#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$ROOT_DIR"

cargo install --path crates/opal --locked --root "$ROOT_DIR/.opal-installed"

"$ROOT_DIR/.opal-installed/bin/opal" run \
  --pipeline "$ROOT_DIR/.gitlab-ci.yml" \
  --workdir "$ROOT_DIR" \
  --base-image "docker.io/library/rust:1.90"
