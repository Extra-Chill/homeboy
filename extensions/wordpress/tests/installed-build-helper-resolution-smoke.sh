#!/usr/bin/env bash
set -euo pipefail

WORDPRESS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_RUNNER="${WORDPRESS_ROOT}/scripts/build/build.sh"
FIXTURE_ROOT="$(mktemp -d)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

mkdir -p \
    "${FIXTURE_ROOT}/extension-sources/wordpress" \
    "${FIXTURE_ROOT}/extensions/nodejs/scripts/lib"
ln -s "${FIXTURE_ROOT}/extension-sources/wordpress" "${FIXTURE_ROOT}/extensions/wordpress"
touch "${FIXTURE_ROOT}/extensions/nodejs/scripts/lib/local-workspace-deps.sh"

EXTENSION_PATH="${FIXTURE_ROOT}/extensions/wordpress"
export EXTENSION_PATH
unset HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS || true
eval "$(awk '/^EXTENSION_COLLECTION_PATH=/{print; next} /^LOCAL_WORKSPACE_DEPS_HELPER=/{print; exit}' "$BUILD_RUNNER")"

EXPECTED="${FIXTURE_ROOT}/extensions/nodejs/scripts/lib/local-workspace-deps.sh"
if [ "$LOCAL_WORKSPACE_DEPS_HELPER" != "$EXPECTED" ] || [ ! -f "$LOCAL_WORKSPACE_DEPS_HELPER" ]; then
    printf 'FAIL: symlinked extension resolved helper to %s\n' "$LOCAL_WORKSPACE_DEPS_HELPER" >&2
    exit 1
fi

printf '%s\n' 'installed build helper resolution smoke passed'
