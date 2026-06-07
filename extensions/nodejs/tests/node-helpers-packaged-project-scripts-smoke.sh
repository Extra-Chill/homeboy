#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_HELPERS="${ROOT_DIR}/scripts/lib/node-helpers.sh"
PROJECT_SCRIPTS="${ROOT_DIR}/scripts/lib/project-scripts.sh"

test -f "$NODE_HELPERS"
test -f "$PROJECT_SCRIPTS"

bash -c '
    source "$1"
    type homeboy_project_init >/dev/null
    type homeboy_project_has_script >/dev/null
    type homeboy_project_run_script_command >/dev/null
    type homeboy_require_package_json >/dev/null
' _ "$NODE_HELPERS"
