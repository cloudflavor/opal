#!/usr/bin/env sh
set -eu
cd /workspace

echo "[global before]"
apt-get update && apt-get -y upgrade
mkdir -p vendor
echo "dependency-output" > vendor/deps.txt
echo "[global after]"
