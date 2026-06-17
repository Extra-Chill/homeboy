#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_HELPERS="${ROOT_DIR}/scripts/lib/node-helpers.sh"
PROJECT_SCRIPTS="${ROOT_DIR}/scripts/lib/project-scripts.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

test -f "$NODE_HELPERS"
test -f "$PROJECT_SCRIPTS"

bash -c '
    source "$1"
    type homeboy_project_init >/dev/null
    type homeboy_project_has_script >/dev/null
    type homeboy_project_run_script_command >/dev/null
    type homeboy_require_package_json >/dev/null
' _ "$NODE_HELPERS"

PNPM_PROJECT="$TMP_DIR/pnpm-project"
mkdir -p "$PNPM_PROJECT/subdir"
cat > "$PNPM_PROJECT/package.json" <<'EOF'
{"name":"packaged-project-helper","scripts":{"test":"node --test"}}
EOF
touch "$PNPM_PROJECT/pnpm-lock.yaml"

bash -c '
    source "$1"
    homeboy_project_init --ecosystem node --path "$2/subdir"
    [ "$HOMEBOY_PROJECT_ROOT" = "$2" ]
    [ "$HOMEBOY_PROJECT_PACKAGE_MANAGER" = "pnpm" ]
    [ "$(homeboy_project_run_script_command test)" = "pnpm run test" ]
' _ "$PROJECT_SCRIPTS" "$PNPM_PROJECT"
