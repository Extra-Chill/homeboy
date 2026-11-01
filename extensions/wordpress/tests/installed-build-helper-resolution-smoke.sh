#!/usr/bin/env bash
set -euo pipefail

# An installed wordpress extension without a sibling nodejs extension (the CI
# layout) must resolve the shared local workspace
# dependency helper from the collection's scripts/lib.
WORDPRESS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_RUNNER="${WORDPRESS_ROOT}/scripts/build/build.sh"
FIXTURE_ROOT="$(mktemp -d)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

mkdir -p \
    "${FIXTURE_ROOT}/extensions/wordpress/scripts/build" \
    "${FIXTURE_ROOT}/extensions/scripts/lib"
touch "${FIXTURE_ROOT}/extensions/scripts/lib/local-workspace-deps.sh"

HOMEBOY_EXTENSION_PATH="${FIXTURE_ROOT}/extensions/wordpress"
export HOMEBOY_EXTENSION_PATH
unset HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS HOMEBOY_SHARED_LIB_DIR || true
SCRIPT_DIR="${FIXTURE_ROOT}/extensions/wordpress/scripts/build"
eval "$(awk '/^SHARED_LIB_DIR=|^if \[ -z "\$SHARED_LIB_DIR" \]|^    SHARED_LIB_DIR=|^fi$/{print} /^LOCAL_WORKSPACE_DEPS_HELPER=/{print; exit}' "$BUILD_RUNNER" | grep -v 'SCRIPT_DIR="$(cd')"

EXPECTED="${FIXTURE_ROOT}/extensions/scripts/lib/local-workspace-deps.sh"
if [ "$LOCAL_WORKSPACE_DEPS_HELPER" != "$EXPECTED" ] || [ ! -f "$LOCAL_WORKSPACE_DEPS_HELPER" ]; then
    printf 'FAIL: installed extension resolved helper to %s\n' "$LOCAL_WORKSPACE_DEPS_HELPER" >&2
    exit 1
fi

printf '%s\n' 'installed build helper resolution smoke passed'
