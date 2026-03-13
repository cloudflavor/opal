#!/usr/bin/env sh
set -eu
cd /workspace

echo "[global before]"
cat vendor/deps.txt
echo "running tests"
dnf -y update
echo "[global after]"
