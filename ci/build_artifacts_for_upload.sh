#!/usr/bin/env bash

# This script will build
#  - debian package
#  - version.json
# in target/artifacts

set -e

if command -v jq >&2; then
  echo "jq installed"
else
  echo "ERROR: jq is not installed and required"
  exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." >/dev/null 2>&1 && pwd)"
SIDECAR_DIR="$ROOT_DIR/sidecar"
ARTIFACT_DIR="$ROOT_DIR/target/artifacts/"
DEBIAN_DIR="$ROOT_DIR/target/debian/"
GIT_HASH=$(git rev-parse HEAD)
BRANCH_NAME=$(git branch --show-current)
SIDECAR_VERSION=$(cat "$SIDECAR_DIR/Cargo.toml" | python3 -c "import sys, toml; print(toml.load(sys.stdin)['package']['version'])")

# Expect debian built in CI
echo "Copying debian"
cd "$ROOT_DIR" || exit
mkdir -p "$ARTIFACT_DIR"
cp "$DEBIAN_DIR"* "$ARTIFACT_DIR" || exit

echo "Building version.json"
jq --null-input \
--arg	branch "$BRANCH_NAME" \
--arg version "$SIDECAR_VERSION" \
--arg ghash "$GIT_HASH" \
--arg now "$(jq -nr 'now | strftime("%Y-%m-%dT%H:%M:%SZ")')" \
--arg files "$(ls "$ARTIFACT_DIR" | jq -nRc '[inputs]')" \
'{"branch": $branch, "version": $version, "git-hash": $ghash, "tag": $tag, "timestamp": $now, "files": $files}' \
> "$ARTIFACT_DIR/version.json"
