#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_PATH="${HOMEBOY_COMPONENT_PATH:-${PROJECT_PATH:-$(pwd)}}"
ACTION="${1:-status}"
PACKAGE_FILTER="${2:-}"

has_composer() {
    [ -f "${PROJECT_PATH}/composer.json" ]
}

has_package_json() {
    [ -f "${PROJECT_PATH}/package.json" ]
}

# Hydration installs skip npm's advisory audit and funding notices, like the
# lockfile-less `npm install` path below. A cold audit fetches full packuments
# for every flagged package and roughly doubles a cold `npm ci`; vulnerability
# reporting belongs to dedicated audit commands, not dependency hydration.
npm_install_command() {
    if [ -f "${PROJECT_PATH}/package-lock.json" ]; then
        printf '%s\n' "npm ci --no-audit --no-fund"
    else
        printf '%s\n' "npm install --no-audit --no-fund"
    fi
}

emit_status() {
    HOMEBOY_WORDPRESS_DEPS_PROJECT_PATH="$PROJECT_PATH" \
    HOMEBOY_WORDPRESS_DEPS_PACKAGE_FILTER="$PACKAGE_FILTER" \
        node <<'NODE'
const fs = require('fs');
const path = require('path');

const projectPath = process.env.HOMEBOY_WORDPRESS_DEPS_PROJECT_PATH;
const filter = process.env.HOMEBOY_WORDPRESS_DEPS_PACKAGE_FILTER || '';
const packages = [];
const dependencyIdentities = [];
const lockfiles = [];

function readJson(file) {
  return JSON.parse(fs.readFileSync(path.join(projectPath, file), 'utf8'));
}

function pushPackage(name, section, constraint, locked) {
  if (!name || (filter && filter !== name)) return;
  const entry = { name, manifest_section: section, constraint: String(constraint) };
  if (locked && locked.version) entry.locked_version = String(locked.version);
  if (locked && locked.reference) entry.locked_reference = String(locked.reference);
  packages.push(entry);
}

// Locked Composer versions come from composer.lock itself, not vendor/, so a
// fresh checkout reports them without installing anything. The reference is
// the resolved source commit (or dist reference when there is no source).
function composerLockedPackages() {
  const locked = new Map();
  if (!fs.existsSync(path.join(projectPath, 'composer.lock'))) return locked;
  let lock;
  try {
    lock = readJson('composer.lock');
  } catch (error) {
    return locked;
  }
  for (const key of ['packages', 'packages-dev']) {
    for (const pkg of lock[key] || []) {
      if (!pkg || !pkg.name) continue;
      locked.set(String(pkg.name), {
        version: pkg.version,
        reference: (pkg.source && pkg.source.reference) || (pkg.dist && pkg.dist.reference) || null,
      });
    }
  }
  return locked;
}

if (fs.existsSync(path.join(projectPath, 'composer.json'))) {
  const composer = readJson('composer.json');
  if (composer.name) dependencyIdentities.push(String(composer.name));
  const locked = composerLockedPackages();
  for (const section of ['require', 'require-dev']) {
    for (const [name, constraint] of Object.entries(composer[section] || {})) {
      pushPackage(name, section, constraint, locked.get(name));
    }
  }
  if (fs.existsSync(path.join(projectPath, 'composer.lock'))) lockfiles.push('composer.lock');
}

if (fs.existsSync(path.join(projectPath, 'package.json'))) {
  const pkg = readJson('package.json');
  if (pkg.name) dependencyIdentities.push(String(pkg.name));
  for (const section of ['dependencies', 'devDependencies', 'peerDependencies', 'optionalDependencies']) {
    for (const [name, constraint] of Object.entries(pkg[section] || {})) {
      pushPackage(name, section, constraint);
    }
  }
  for (const lockfile of ['pnpm-lock.yaml', 'yarn.lock', 'package-lock.json']) {
    if (fs.existsSync(path.join(projectPath, lockfile))) lockfiles.push(lockfile);
  }
}

process.stdout.write(JSON.stringify({
  package_manager: 'wordpress',
  dependency_identities: [...new Set(dependencyIdentities)],
  lockfiles,
  packages,
  errors: [],
}) + '\n');
NODE
}

emit_install_command() {
    HOMEBOY_WORDPRESS_DEPS_RUNNER="${SCRIPT_DIR}/deps-runner.sh" node <<'NODE'
process.stdout.write(JSON.stringify({
  command: ['bash', process.env.HOMEBOY_WORDPRESS_DEPS_RUNNER, 'install'],
}) + '\n');
NODE
}

# The npm install bridge is a shared asset (extensions/homeboy-extension-root.json
# ships scripts/lib next to the installed extensions). An install source that
# predates it, or overwrote the shared assets, leaves it missing; name the path
# and the remedy instead of dying on a bare `source` error mid-install.
SHARED_INSTALL_HELPER="$(cd "${SCRIPT_DIR}/../../.." && pwd)/scripts/lib/local-workspace-install.sh"

require_shared_install_helper() {
    [ -f "$SHARED_INSTALL_HELPER" ] && return 0
    echo "ERROR: Homeboy shared extension helper is missing: ${SHARED_INSTALL_HELPER}" >&2
    echo "  The shared scripts/lib assets installed next to the extensions are incomplete, usually because an older install source replaced them." >&2
    echo "  Reinstall extensions from the Homeboy source, e.g.: homeboy extension install https://github.com/Extra-Chill/homeboy --id wordpress" >&2
    exit 1
}

run_composer_install() (
    cd "$PROJECT_PATH" || exit 1
    # Lockless installs resolve dependencies and can write a lockfile even if
    # installation fails. Keep vendor/ but leave the release source lockless.
    if [ ! -e composer.lock ] && [ ! -L composer.lock ]; then
        trap 'rm -f -- composer.lock' EXIT
    fi
    composer install --no-interaction --prefer-dist
)

run_install() {
    local ran=0
    if has_composer; then
        echo "Installing WordPress PHP dependencies with composer" >&2
        run_composer_install
        ran=1
    fi
    if has_package_json; then
        local command
        command="$(npm_install_command)"
        echo "Installing WordPress JavaScript dependencies: ${command}" >&2
        require_shared_install_helper
        # shellcheck source=/dev/null
        source "$SHARED_INSTALL_HELPER"
        # Declared local workspace overrides are installed before the
        # consumer's lifecycle (`prepare`) scripts run (homeboy#18302).
        # shellcheck disable=SC2086
        homeboy_npm_install_with_local_workspace "$PROJECT_PATH" "$PROJECT_PATH" "${SCRIPT_DIR}/../../.." $command
        ran=1
    fi
    if [ "$ran" -eq 0 ]; then
        echo "No composer.json or package.json found in ${PROJECT_PATH}; nothing to install" >&2
    fi
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
        echo "wordpress deps update is not implemented" >&2
        exit 64
        ;;
    *)
        echo "unknown wordpress deps action: $ACTION" >&2
        exit 64
        ;;
esac
