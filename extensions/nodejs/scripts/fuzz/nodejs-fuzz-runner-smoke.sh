#!/usr/bin/env bash
# shellcheck disable=SC2016
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_DIR}/../.." && pwd)/homeboy}"
REPOSITORY_ROOT="$(cd "${EXTENSION_DIR}/.." && pwd)"
# Helpers resolve through the shared resolver. The literal path used
# here previously pointed at src/core/extension/runtime, a Homeboy layout
# that has not existed since the move to crates/.
# shellcheck source=/dev/null
source "${REPOSITORY_ROOT}/scripts/lib/runtime-helper-resolver.sh"
BASH_PREFLIGHT_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_BASH_PREFLIGHT bash-preflight.sh)" || exit 1
RESOLVE_CONTEXT_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)" || exit 1
MANIFEST="${EXTENSION_DIR}/nodejs.json"
RUNNER="${SCRIPT_DIR}/fuzz-runner.sh"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-fuzz.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

if [ ! -f "$BASH_PREFLIGHT_HELPER" ]; then
    echo "Missing bash preflight helper: $BASH_PREFLIGHT_HELPER" >&2
    exit 1
fi
if [ ! -f "$RESOLVE_CONTEXT_HELPER" ]; then
    echo "Missing resolve context helper: $RESOLVE_CONTEXT_HELPER" >&2
    exit 1
fi

assert_json() {
    local file="$1"
    local script="$2"
    node -e "const fs = require('fs'); const data = JSON.parse(fs.readFileSync(process.argv[1], 'utf8')); ${script}" "$file"
}

make_project() {
    local name="$1"
    local project_dir="$TMP_DIR/$name"
    mkdir -p "$project_dir"
    cat > "$project_dir/package.json" <<'JSON'
{"name":"node-fuzz-smoke","scripts":{"fuzz":"node fuzz-package.mjs","fuzz:configured":"node configured-fuzz.mjs"}}
JSON
    printf '%s\n' "$project_dir"
}

run_fuzz() {
    local project_dir="$1"
    local results_file="$2"
    local workload_path="${3:-}"
    local runner="${4:-$RUNNER}"
    local extension_dir="${5:-$EXTENSION_DIR}"
    HOMEBOY_EXTENSION_PATH="$extension_dir" \
    HOMEBOY_COMPONENT_PATH="$project_dir" \
    HOMEBOY_COMPONENT_ID="node-fuzz-smoke" \
    HOMEBOY_FUZZ_RESULTS_FILE="$results_file" \
    HOMEBOY_FUZZ_ARTIFACTS_DIR="$TMP_DIR/artifacts" \
    HOMEBOY_FUZZ_WORKLOAD_PATH="$workload_path" \
    HOMEBOY_RUNTIME_BASH_PREFLIGHT="$BASH_PREFLIGHT_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
        bash "$runner" --flag
}

node - "$MANIFEST" "$RUNNER" <<'NODE'
const fs = require('fs');
const [manifestPath, runnerPath] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
const runner = fs.readFileSync(runnerPath, 'utf8');
// Fuzz support is advertised by the top-level `fuzz` block, which is what
// ExtensionManifest::has_fuzz reads. `provides.capabilities` is not a field any
// manifest in this repository declares, so the previous assertion could never
// pass — it went unnoticed because this smoke could not run at all.
if (!manifest.fuzz) throw new Error('Node.js manifest does not advertise fuzz capability');
if (manifest.fuzz?.extension_script !== 'scripts/fuzz/fuzz-runner.sh') throw new Error('fuzz runner path is not declared in nodejs.json');
if (!manifest.fuzz?.capabilities?.includes('nodejs-fuzz-workload')) throw new Error('Node.js fuzz workload capability missing');
if (!manifest.fuzz?.runtime_helpers?.some((helper) => helper.id === 'runtime-settings')) throw new Error('Node.js fuzz runner must declare the runtime-settings helper capability');
for (const token of [
  'HOMEBOY_FUZZ_RESULTS_FILE',
  'HOMEBOY_FUZZ_WORKLOAD_PATH',
  'HOMEBOY_FUZZ_ARTIFACTS_DIR',
  'HOMEBOY_NODE_FUZZ_SCRIPT',
  'HOMEBOY_NODE_FUZZ_COMMAND',
]) {
  if (!runner.includes(token)) throw new Error(`runner missing env contract token ${token}`);
}
NODE

SCRIPT_PROJECT="$(make_project script-workload)"
WORKLOAD_SCRIPT="$SCRIPT_PROJECT/rig-fuzz.mjs"
cat > "$WORKLOAD_SCRIPT" <<'JS'
import { writeFileSync } from 'node:fs';
writeFileSync(process.env.HOMEBOY_FUZZ_RESULTS_FILE, JSON.stringify({
  schema: 'homeboy/fuzz-campaign/v1',
  status: 'pass',
  source: 'script-workload',
  args: process.argv.slice(2),
}, null, 2));
JS
SCRIPT_RESULTS="$TMP_DIR/script-results.json"
run_fuzz "$SCRIPT_PROJECT" "$SCRIPT_RESULTS" "$WORKLOAD_SCRIPT" >/dev/null
assert_json "$SCRIPT_RESULTS" '
if (data.schema !== "homeboy/fuzz-campaign/v1") throw new Error("script workload schema missing");
if (data.source !== "script-workload") throw new Error("script workload did not run");
if (data.args.join(",") !== "--flag") throw new Error(`script args not forwarded: ${data.args.join(",")}`);
'

ISOLATED_EXTENSION="$TMP_DIR/isolated/nodejs"
mkdir -p "$ISOLATED_EXTENSION/scripts/fuzz" "$ISOLATED_EXTENSION/scripts/lib" \
    "$TMP_DIR/isolated/scripts/lib"
cp "$RUNNER" "$ISOLATED_EXTENSION/scripts/fuzz/fuzz-runner.sh"
cp "$EXTENSION_DIR/scripts/lib/node-helpers.sh" "$ISOLATED_EXTENSION/scripts/lib/node-helpers.sh"
# An installed extension sits beside the shared lib directory
# (extensions/nodejs and extensions/scripts/lib), so the fixture reproduces
# that sibling rather than only the extension's own scripts/lib.
cp "$REPOSITORY_ROOT/scripts/lib/runner-harness.sh" \
    "$REPOSITORY_ROOT/scripts/lib/runtime-helper-resolver.sh" \
    "$TMP_DIR/isolated/scripts/lib/"
ISOLATED_RESULTS="$TMP_DIR/isolated-results.json"
# The point of this case is that an extension copied outside the repository
# still runs against the core-declared settings helper. Resolve it the same way
# the runner does rather than demanding the caller export it.
ISOLATED_SETTINGS_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_SETTINGS_HELPER settings.sh)" || exit 1
HOMEBOY_RUNTIME_SETTINGS_HELPER="$ISOLATED_SETTINGS_HELPER" \
HOMEBOY_RUNTIME_PROJECT_SCRIPTS="$EXTENSION_DIR/../scripts/lib/project-scripts.sh" \
    run_fuzz "$SCRIPT_PROJECT" "$ISOLATED_RESULTS" "$WORKLOAD_SCRIPT" \
        "$ISOLATED_EXTENSION/scripts/fuzz/fuzz-runner.sh" "$ISOLATED_EXTENSION" >/dev/null
assert_json "$ISOLATED_RESULTS" '
if (data.source !== "script-workload") throw new Error("isolated extension script workload did not run");
'

PACKAGE_PROJECT="$(make_project package-script)"
cat > "$PACKAGE_PROJECT/fuzz-package.mjs" <<'JS'
import { writeFileSync } from 'node:fs';
writeFileSync(process.env.HOMEBOY_FUZZ_RESULTS_FILE, JSON.stringify({
  schema: 'homeboy/fuzz-campaign/v1',
  status: 'pass',
  source: 'package-script',
  args: process.argv.slice(2),
}, null, 2));
JS
PACKAGE_RESULTS="$TMP_DIR/package-results.json"
run_fuzz "$PACKAGE_PROJECT" "$PACKAGE_RESULTS" >/dev/null
assert_json "$PACKAGE_RESULTS" '
if (data.source !== "package-script") throw new Error("package scripts.fuzz did not run");
if (data.args.join(",") !== "--flag") throw new Error(`package args not forwarded: ${data.args.join(",")}`);
'

CONFIG_PROJECT="$(make_project configured-script)"
cat > "$CONFIG_PROJECT/configured-fuzz.mjs" <<'JS'
import { writeFileSync } from 'node:fs';
writeFileSync(process.env.HOMEBOY_FUZZ_RESULTS_FILE, JSON.stringify({
  schema: 'homeboy/fuzz-campaign/v1',
  status: 'pass',
  source: 'configured-script',
}, null, 2));
JS
CONFIG_RESULTS="$TMP_DIR/config-results.json"
HOMEBOY_NODE_FUZZ_SCRIPT="fuzz:configured" run_fuzz "$CONFIG_PROJECT" "$CONFIG_RESULTS" >/dev/null
assert_json "$CONFIG_RESULTS" '
if (data.source !== "configured-script") throw new Error("configured fuzz script did not run");
'

JSON_PROJECT="$(make_project json-workload)"
cat > "$JSON_PROJECT/json-fuzz.mjs" <<'JS'
import { writeFileSync } from 'node:fs';
writeFileSync(process.env.HOMEBOY_FUZZ_RESULTS_FILE, JSON.stringify({
  schema: 'homeboy/fuzz-campaign/v1',
  status: 'pass',
  source: 'json-workload',
  args: process.argv.slice(2),
}, null, 2));
JS
JSON_WORKLOAD="$JSON_PROJECT/workload.json"
cat > "$JSON_WORKLOAD" <<'JSON'
{"path":"json-fuzz.mjs","args":["--from-json"]}
JSON
JSON_RESULTS="$TMP_DIR/json-results.json"
run_fuzz "$JSON_PROJECT" "$JSON_RESULTS" "$JSON_WORKLOAD" >/dev/null
assert_json "$JSON_RESULTS" '
if (data.source !== "json-workload") throw new Error("json workload path did not run");
if (data.args.join(",") !== "--from-json,--flag") throw new Error(`json workload args not forwarded: ${data.args.join(",")}`);
'

NESTED_JSON_PROJECT="$(make_project nested-json-workload)"
cat > "$NESTED_JSON_PROJECT/nested-json-fuzz.mjs" <<'JS'
import { existsSync, writeFileSync } from 'node:fs';
writeFileSync(process.env.HOMEBOY_FUZZ_RESULTS_FILE, JSON.stringify({
  schema: 'homeboy/fuzz-campaign/v1',
  status: 'pass',
  source: 'nested-json-workload',
  args: process.argv.slice(2),
  injectedFileExists: existsSync('nested-injected'),
}, null, 2));
JS
NESTED_JSON_WORKLOAD="$NESTED_JSON_PROJECT/nested-workload.json"
cat > "$NESTED_JSON_WORKLOAD" <<'JSON'
{
  "schema": "homeboy/fuzz-workload/v1",
  "workload": {
    "path": "nested-json-fuzz.mjs",
    "args": ["--from-nested", "value with spaces", "; touch nested-injected"]
  }
}
JSON
NESTED_JSON_RESULTS="$TMP_DIR/nested-json-results.json"
run_fuzz "$NESTED_JSON_PROJECT" "$NESTED_JSON_RESULTS" "$NESTED_JSON_WORKLOAD" >/dev/null
assert_json "$NESTED_JSON_RESULTS" '
if (data.source !== "nested-json-workload") throw new Error("nested json workload path did not run");
if (data.args.join("|") !== "--from-nested|value with spaces|; touch nested-injected|--flag") throw new Error(`nested json workload args not forwarded safely: ${data.args.join("|")}`);
if (data.injectedFileExists) throw new Error("nested json workload args were evaluated by the shell");
'

echo "Node.js fuzz runner smoke passed."
