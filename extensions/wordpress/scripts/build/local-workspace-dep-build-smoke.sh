#!/usr/bin/env bash
set -euo pipefail

# End-to-end smoke for the WordPress build's local-workspace-dependency override.
#
# Runs the real build.sh against a plugin that declares a local_workspace_dependencies
# override consuming a locally "cooked" (unpublished) sibling package. Proves:
#   * build.sh builds + packs + installs the local dependency before the consumer build
#   * the consumer build can resolve the BUILT dependency
#   * peer deps (React) dedupe (no nested second copy)
#   * a declared override with a missing helper fails the build loudly

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_CORE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wordpress-lwd.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

if ! command -v npm >/dev/null 2>&1; then
    echo "npm is required for the WordPress local workspace deps smoke" >&2
    exit 1
fi

fail() {
    echo "FAIL: $1" >&2
    exit 1
}

# --- Fixture: peer dependency (fake React) -------------------------------------
react_dir="${TMP_DIR}/react-fake"
mkdir -p "$react_dir"
cat > "${react_dir}/package.json" <<'JSON'
{ "name": "react", "version": "18.3.0", "main": "index.js" }
JSON
printf 'module.exports = { version: "18.3.0" };\n' > "${react_dir}/index.js"

# --- Fixture: locally cooked dependency (monorepo subpackage) -------------------
monorepo_dir="${TMP_DIR}/monorepo"
widget_dir="${monorepo_dir}/packages/widget"
mkdir -p "${widget_dir}"
cat > "${widget_dir}/package.json" <<'JSON'
{
  "name": "@scope/widget",
  "version": "1.0.0",
  "main": "dist/index.js",
  "files": ["dist", "package.json"],
  "scripts": { "build": "node build.js" },
  "peerDependencies": { "react": "*" }
}
JSON
cat > "${widget_dir}/build.js" <<'JS'
const fs = require('fs');
fs.mkdirSync('dist', { recursive: true });
fs.writeFileSync('dist/index.js', 'module.exports = { widget: "built" };\n');
JS

# --- Fixture: the WordPress plugin (consumer) ----------------------------------
component_dir="${TMP_DIR}/lwd-plugin"
mkdir -p "$component_dir"
cat > "${component_dir}/lwd-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: LWD Plugin
 * Version: 1.0.0
 */
PHP

# The consumer "build" verifies the BUILT dependency resolved and that React was
# deduped (no nested copy), then writes a marker that lands in build/.
cat > "${component_dir}/verify-build.js" <<'JS'
const fs = require('fs');
const built = require('@scope/widget');
if (built.widget !== 'built') {
  console.error('dependency is not the built artifact');
  process.exit(1);
}
if (fs.existsSync('node_modules/@scope/widget/node_modules/react')) {
  console.error('duplicate React under dependency — peer dedup failed');
  process.exit(1);
}
fs.mkdirSync('build', { recursive: true });
fs.writeFileSync('build/ok.txt', 'built+deduped\n');
JS

cat > "${component_dir}/package.json" <<JSON
{
  "name": "lwd-plugin",
  "version": "1.0.0",
  "private": true,
  "scripts": { "build": "node verify-build.js" },
  "dependencies": { "react": "file:${react_dir}" }
}
JSON

settings_json="$(node -e '
  process.stdout.write(JSON.stringify({
    local_workspace_dependencies: [
      { name: "@scope/widget", path: "'"${monorepo_dir}"'", package_dir: "packages/widget" }
    ]
  }));
')"

# --- Run the real build --------------------------------------------------------
build_out="${TMP_DIR}/build.out"
(
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="lwd-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_REQUIRE_FRONTEND=1 \
    HOMEBOY_SETTINGS_JSON="$settings_json" \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "$build_out" 2>&1
) || { sed 's/^/  /' "$build_out" >&2; fail "build with local workspace override failed"; }

grep -Fq '[local-workspace-deps]' "$build_out" || {
    sed 's/^/  /' "$build_out" >&2
    fail "override helper did not run during build"
}

zip_file="${component_dir}/build/lwd-plugin.zip"
[ -f "$zip_file" ] || fail "build artifact missing: $zip_file"
unzip -l "$zip_file" | grep -Fq 'lwd-plugin/build/ok.txt' \
    || { unzip -l "$zip_file" >&2; fail "consumer build output (proving resolved+deduped dependency) missing from zip"; }

echo "ok: build.sh built, packed, installed and deduped the local workspace dependency"

# --- Declared override but helper missing must fail loudly ---------------------
missing_out="${TMP_DIR}/missing.out"
if (
    cd "$component_dir"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_ID="lwd-plugin" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
    HOMEBOY_SKIP_TESTS=1 \
    HOMEBOY_SETTINGS_JSON="$settings_json" \
    HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS="${TMP_DIR}/does-not-exist.sh" \
        bash "${EXTENSION_DIR}/scripts/build/build.sh" > "$missing_out" 2>&1
); then
    sed 's/^/  /' "$missing_out" >&2
    fail "build should fail when a declared override's helper is missing"
fi
grep -Fq 'helper not found' "$missing_out" \
    || { sed 's/^/  /' "$missing_out" >&2; fail "expected 'helper not found' error"; }

echo "ok: declared override with missing helper fails the build"

echo "WordPress local workspace dependency build smoke passed."
