#!/usr/bin/env bash

if [ -z "${BASH_VERSION:-}" ]; then
    exec bash "$0" "$@"
fi

set -euo pipefail

# Smoke test for the generic local-workspace-dependency override helper.
#
# Proves the real mechanism with real npm:
#   1. A locally "cooked" (unpublished) sibling package is BUILT from source.
#   2. It is packed into a self-contained tarball and installed into a consumer.
#   3. The consumer ends up with the BUILT dependency and a single, deduped copy
#      of the peer dependency (React) — NOT a nested second copy that a `file:`
#      symlink would produce.
# Also exercises config validation error paths.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
HELPER="${EXTENSION_DIR}/scripts/lib/local-workspace-deps.sh"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-lwd-smoke.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

if ! command -v npm >/dev/null 2>&1; then
    echo "npm is required for the local workspace deps smoke" >&2
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

# --- Fixture: local workspace dependency (a monorepo subpackage) ---------------
# Source lives under monorepo/packages/widget; it declares React as a PEER dep,
# has a real build step that emits dist/, and is unpublished.
monorepo_dir="${TMP_DIR}/monorepo"
widget_dir="${monorepo_dir}/packages/widget"
mkdir -p "${widget_dir}/src"
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
printf 'export const widget = "src";\n' > "${widget_dir}/src/index.js"
cat > "${widget_dir}/build.js" <<'JS'
const fs = require('fs');
fs.mkdirSync('dist', { recursive: true });
fs.writeFileSync('dist/index.js', 'module.exports = { widget: "built" };\n');
JS

# --- Fixture: consumer ---------------------------------------------------------
consumer_dir="${TMP_DIR}/consumer"
mkdir -p "$consumer_dir"
cat > "${consumer_dir}/package.json" <<JSON
{
  "name": "consumer",
  "version": "1.0.0",
  "private": true,
  "dependencies": { "react": "file:${react_dir}" }
}
JSON

# Install the consumer's own deps first (React must exist to dedupe against).
( cd "$consumer_dir" && npm install --no-audit --no-fund >/dev/null 2>&1 ) \
    || fail "consumer npm install failed"
[ -f "${consumer_dir}/node_modules/react/index.js" ] || fail "consumer React not installed"

# --- Apply the override --------------------------------------------------------
settings_json="$(node -e '
  process.stdout.write(JSON.stringify({
    local_workspace_dependencies: [
      { name: "@scope/widget", path: "'"${monorepo_dir}"'", package_dir: "packages/widget" }
    ]
  }));
')"

out_file="${TMP_DIR}/apply.out"
(
    HOMEBOY_SETTINGS_JSON="$settings_json" \
        bash -c 'source "$1"; homeboy_apply_local_workspace_dependencies "$2"' \
        _ "$HELPER" "$consumer_dir"
) > "$out_file" 2>&1 || { sed 's/^/  /' "$out_file" >&2; fail "override application failed"; }

# The BUILT dependency must be installed in the consumer.
dep_built="${consumer_dir}/node_modules/@scope/widget/dist/index.js"
[ -f "$dep_built" ] || { sed 's/^/  /' "$out_file" >&2; fail "built dependency not installed into consumer"; }
grep -Fq 'built' "$dep_built" || fail "installed dependency is not the BUILT artifact"

# It must be a real installed package, NOT a live symlink into the source tree.
if [ -L "${consumer_dir}/node_modules/@scope/widget" ]; then
    fail "dependency installed as a symlink — peer deps would NOT dedupe"
fi

# Peer dedup: there must be NO second React nested under the dependency.
if [ -e "${consumer_dir}/node_modules/@scope/widget/node_modules/react" ]; then
    fail "duplicate React under dependency — peer dedup failed (Invalid hook call risk)"
fi

# Structured evidence line emitted.
grep -Fq '"type":"nodejs.local_workspace_dependency"' "$out_file" \
    || fail "missing structured evidence line"
grep -Fq '"name":"@scope/widget"' "$out_file" || fail "evidence missing dependency name"

echo "ok: real build + pack + install with peer dedup"

# --- No-op cases ---------------------------------------------------------------
run_helper() {
    local settings="$1"
    local consumer="$2"
    HOMEBOY_SETTINGS_JSON="$settings" \
        bash -c 'source "$1"; homeboy_apply_local_workspace_dependencies "$2"' \
        _ "$HELPER" "$consumer" 2>&1
}

run_helper "" "$consumer_dir" >/dev/null || fail "empty settings should be a no-op"
run_helper "{}" "$consumer_dir" >/dev/null || fail "empty object should be a no-op"
run_helper '{"package_artifacts":["x"]}' "$consumer_dir" >/dev/null \
    || fail "unrelated settings should be a no-op"
echo "ok: no-op when no overrides declared"

# --- Validation error cases ----------------------------------------------------
expect_failure() {
    local label="$1"
    local settings="$2"
    local needle="$3"
    local result
    if result="$(run_helper "$settings" "$consumer_dir")"; then
        echo "$result" | sed 's/^/  /' >&2
        fail "expected failure: $label"
    fi
    grep -Fq "$needle" <<<"$result" || {
        echo "$result" | sed 's/^/  /' >&2
        fail "expected error message for $label to contain: $needle"
    }
}

expect_failure "missing name" \
    '{"local_workspace_dependencies":[{"path":"../x"}]}' \
    '"name" must be a non-empty string'

expect_failure "non-array" \
    '{"local_workspace_dependencies":{"name":"x"}}' \
    'local_workspace_dependencies must be an array'

expect_failure "missing source path" \
    '{"local_workspace_dependencies":[{"name":"@scope/widget","path":"./does-not-exist"}]}' \
    'source directory not found'

expect_failure "name mismatch" \
    "$(node -e 'process.stdout.write(JSON.stringify({local_workspace_dependencies:[{name:"@scope/WRONG",path:"'"${monorepo_dir}"'",package_dir:"packages/widget"}]}))')" \
    "expected '@scope/WRONG'"

echo "ok: config validation rejects malformed overrides"

echo "local workspace deps smoke passed."
