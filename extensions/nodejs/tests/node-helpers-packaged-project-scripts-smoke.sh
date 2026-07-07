#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "${ROOT_DIR}/.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

INSTALL_ROOT="$TMP_DIR/home/.config/homeboy/extensions"
mkdir -p "$INSTALL_ROOT"
cp -R "$ROOT_DIR" "$INSTALL_ROOT/nodejs"
cp -R "$REPO_ROOT/scripts" "$INSTALL_ROOT/scripts"
cp -R "$REPO_ROOT/dependency-adapters" "$INSTALL_ROOT/dependency-adapters"

NODE_HELPERS="${INSTALL_ROOT}/nodejs/scripts/lib/node-helpers.sh"
PROJECT_SCRIPTS="${INSTALL_ROOT}/scripts/lib/project-scripts.sh"

test -f "$NODE_HELPERS"
test -f "$PROJECT_SCRIPTS"
test -f "${INSTALL_ROOT}/dependency-adapters/examples/nodejs.json"

bash -c '
    source "$1"
    type homeboy_project_init >/dev/null
    type homeboy_project_has_script >/dev/null
    type homeboy_project_run_script_command >/dev/null
    type homeboy_require_package_json >/dev/null
' _ "$NODE_HELPERS"

DEV_ROOT="$TMP_DIR/dev-overlay-src"
mkdir -p "$DEV_ROOT"
cp -R "$ROOT_DIR" "$DEV_ROOT/nodejs"
rm -rf "$INSTALL_ROOT/nodejs"
ln -s "$DEV_ROOT/nodejs" "$INSTALL_ROOT/nodejs"
DEV_NODE_HELPERS="${INSTALL_ROOT}/nodejs/scripts/lib/node-helpers.sh"

test -f "$DEV_NODE_HELPERS"

bash -c '
    export HOMEBOY_EXTENSION_PATH="$1/nodejs"
    source "$2"
    type homeboy_project_init >/dev/null
    type homeboy_project_has_script >/dev/null
    type homeboy_project_run_script_command >/dev/null
    type homeboy_require_package_json >/dev/null
' _ "$INSTALL_ROOT" "$DEV_NODE_HELPERS"

PNPM_PROJECT="$TMP_DIR/pnpm-project"
mkdir -p "$PNPM_PROJECT/subdir"
cat > "$PNPM_PROJECT/package.json" <<'EOF'
{"name":"packaged-project-helper","scripts":{"test":"node --test"}}
EOF
touch "$PNPM_PROJECT/pnpm-lock.yaml"

bash -c '
    source "$1"
    homeboy_project_init --ecosystem nodejs --path "$2/subdir"
    [ "$HOMEBOY_PROJECT_ROOT" = "$2" ]
    [ "$HOMEBOY_PROJECT_DEPENDENCY_ROOT" = "$2" ]
    [ "$HOMEBOY_PROJECT_PACKAGE_MANAGER" = "pnpm" ]
    [ "$(homeboy_project_run_script_command test)" = "pnpm run test" ]
' _ "$PROJECT_SCRIPTS" "$PNPM_PROJECT"

WORKSPACE_PROJECT="$TMP_DIR/workspace-project"
mkdir -p "$WORKSPACE_PROJECT/packages/plugin/subdir"
cat > "$WORKSPACE_PROJECT/package.json" <<'EOF'
{"name":"workspace-root","scripts":{"root":"node root.js"}}
EOF
cat > "$WORKSPACE_PROJECT/packages/plugin/package.json" <<'EOF'
{"name":"workspace-plugin","scripts":{"test":"node plugin.js"}}
EOF
touch "$WORKSPACE_PROJECT/pnpm-lock.yaml" "$WORKSPACE_PROJECT/pnpm-workspace.yaml"

bash -c '
    source "$1"
    homeboy_project_init --ecosystem nodejs --path "$2/packages/plugin/subdir"
    [ "$HOMEBOY_PROJECT_ROOT" = "$2/packages/plugin" ]
    [ "$HOMEBOY_PROJECT_DEPENDENCY_ROOT" = "$2" ]
    [ "$HOMEBOY_PROJECT_PACKAGE_MANAGER" = "pnpm" ]
    [ "$(homeboy_project_run_script_command test)" = "pnpm run test" ]
' _ "$PROJECT_SCRIPTS" "$WORKSPACE_PROJECT"
