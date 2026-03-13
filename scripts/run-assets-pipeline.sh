#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$ROOT_DIR"

cargo build --release

"$ROOT_DIR/target/release/opal" run \
  --pipeline "$ROOT_DIR/assets/.gitlab-ci.yml" \
  --workdir "$ROOT_DIR/assets" \
  --base-image "alpine:latest"
