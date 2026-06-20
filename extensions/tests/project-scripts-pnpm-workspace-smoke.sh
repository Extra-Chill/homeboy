#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT_SCRIPTS="${ROOT_DIR}/scripts/lib/project-scripts.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

WORKSPACE_PROJECT="$TMP_DIR/workspace-project"
mkdir -p "$WORKSPACE_PROJECT/packages/plugin/subdir"
cat > "$WORKSPACE_PROJECT/package.json" <<'EOF'
{"name":"workspace-root"}
EOF
cat > "$WORKSPACE_PROJECT/packages/plugin/package.json" <<'EOF'
{"name":"workspace-plugin","scripts":{"test":"node plugin.js"}}
EOF
touch "$WORKSPACE_PROJECT/pnpm-lock.yaml" "$WORKSPACE_PROJECT/pnpm-workspace.yaml"

bash -c '
    source "$1"
    homeboy_project_init --ecosystem node --path "$2/packages/plugin/subdir"
    [ "$HOMEBOY_PROJECT_ROOT" = "$2/packages/plugin" ]
    [ "$HOMEBOY_PROJECT_DEPENDENCY_ROOT" = "$2" ]
    [ "$HOMEBOY_PROJECT_PACKAGE_MANAGER" = "pnpm" ]
    [ "$(homeboy_project_run_script_command test)" = "pnpm run test" ]
' _ "$PROJECT_SCRIPTS" "$WORKSPACE_PROJECT"
