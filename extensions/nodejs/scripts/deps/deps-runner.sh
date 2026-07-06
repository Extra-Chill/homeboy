#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/node-helpers.sh
source "${SCRIPT_DIR}/../lib/node-helpers.sh"

PROJECT_PATH="${HOMEBOY_COMPONENT_PATH:-${PROJECT_PATH:-$(pwd)}}"
ACTION="${1:-status}"
PACKAGE_FILTER="${2:-}"

homeboy_require_package_json "$PROJECT_PATH" >/dev/null
homeboy_detect_package_manager "$PROJECT_PATH" >/dev/null

dependency_command() {
    case "$HOMEBOY_PROJECT_PACKAGE_MANAGER" in
        pnpm)
            printf '%s\n' "pnpm install --frozen-lockfile"
            ;;
        yarn)
            printf '%s\n' "yarn install --frozen-lockfile"
            ;;
        npm|*)
            if [ -f "${HOMEBOY_PROJECT_DEPENDENCY_ROOT}/package-lock.json" ]; then
                printf '%s\n' "npm ci"
            else
                printf '%s\n' "npm install"
            fi
            ;;
    esac
}

emit_status() {
    HOMEBOY_NODE_DEPS_PACKAGE_MANAGER="$HOMEBOY_PROJECT_PACKAGE_MANAGER" \
    HOMEBOY_NODE_DEPS_PACKAGE_JSON="${HOMEBOY_PROJECT_ROOT}/package.json" \
    HOMEBOY_NODE_DEPS_PACKAGE_FILTER="$PACKAGE_FILTER" \
        node <<'NODE'
const fs = require('fs');
const pkg = JSON.parse(fs.readFileSync(process.env.HOMEBOY_NODE_DEPS_PACKAGE_JSON, 'utf8'));
const filter = process.env.HOMEBOY_NODE_DEPS_PACKAGE_FILTER || '';
const sections = ['dependencies', 'devDependencies', 'peerDependencies', 'optionalDependencies'];
const packages = [];
for (const section of sections) {
  const deps = pkg[section] || {};
  for (const [name, constraint] of Object.entries(deps)) {
    if (filter && filter !== name) continue;
    packages.push({ name, manifest_section: section, constraint: String(constraint) });
  }
}
process.stdout.write(JSON.stringify({
  package_manager: process.env.HOMEBOY_NODE_DEPS_PACKAGE_MANAGER || 'npm',
  dependency_identities: pkg.name ? [String(pkg.name)] : [],
  packages,
}) + '\n');
NODE
}

emit_install_command() {
    HOMEBOY_NODE_DEPS_COMMAND="$(dependency_command)" node <<'NODE'
const command = (process.env.HOMEBOY_NODE_DEPS_COMMAND || '').split(/\s+/).filter(Boolean);
process.stdout.write(JSON.stringify({ command }) + '\n');
NODE
}

run_install() {
    local command
    command="$(dependency_command)"
    echo "Installing Node.js dependencies with ${HOMEBOY_PROJECT_PACKAGE_MANAGER}: ${command}" >&2
    cd "$HOMEBOY_PROJECT_DEPENDENCY_ROOT"
    # Command strings are extension-owned package-manager argv selected from a
    # fixed allowlist above; run through the shell so flags stay readable.
    $command
}

case "$ACTION" in
    status)
        emit_status
        ;;
    install-command)
        emit_install_command
        ;;
    install)
        run_install
        ;;
    update)
        echo "nodejs deps update is not implemented" >&2
        exit 64
        ;;
    *)
        echo "unknown nodejs deps action: $ACTION" >&2
        exit 64
        ;;
esac
