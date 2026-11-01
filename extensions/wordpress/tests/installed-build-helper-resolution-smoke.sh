#!/usr/bin/env bash
set -euo pipefail

WORDPRESS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_RUNNER="${WORDPRESS_ROOT}/scripts/build/build.sh"
source "${WORDPRESS_ROOT}/../scripts/lib/runtime-helper-resolver.sh"
HELPER="$(homeboy_runtime_helper "${WORDPRESS_ROOT}/.." HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS local-workspace-deps.sh)"
FIXTURE_ROOT="$(mktemp -d)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

mkdir -p "${FIXTURE_ROOT}/extension-sources/wordpress" "${FIXTURE_ROOT}/extensions" "${FIXTURE_ROOT}/runtime"
ln -s "${FIXTURE_ROOT}/extension-sources/wordpress" "${FIXTURE_ROOT}/extensions/wordpress"
cp "$HELPER" "${FIXTURE_ROOT}/runtime/local-workspace-deps.sh"
export EXTENSION_PATH="${FIXTURE_ROOT}/extensions/wordpress"
export HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS="${FIXTURE_ROOT}/runtime/local-workspace-deps.sh"
eval "$(awk '/^LOCAL_WORKSPACE_DEPS_HELPER=/{print; exit}' "$BUILD_RUNNER")"

[ ! -e "${FIXTURE_ROOT}/extensions/nodejs" ]
[ "$LOCAL_WORKSPACE_DEPS_HELPER" = "$HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS" ]
source "$LOCAL_WORKSPACE_DEPS_HELPER"
declare -F homeboy_apply_local_workspace_dependencies >/dev/null
printf '%s\n' 'installed WordPress-only build helper resolution smoke passed'
