#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner-wp-codebox.sh"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

PLUGIN_PATH="${TMPDIR}/component"
DEP_PATH="${TMPDIR}/dep-plugin@branch"
REMOTE_DEP_PATH="${TMPDIR}/remote/dep-plugin"
FAKE_WP_CODEBOX="${TMPDIR}/wp-codebox.js"
ARGS_FILE="${TMPDIR}/wp-codebox-args.txt"
ARTIFACTS_DIR="${TMPDIR}/artifacts"
WRITABLE_DIR="${TMPDIR}/writable"
STUBS_DIR="${TMPDIR}/stubs"
COMPOSER_LOG="${TMPDIR}/composer.log"
: > "$COMPOSER_LOG"
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context-helper.sh"
WRITE_RESULTS_HELPER="${TMPDIR}/write-test-results-helper.sh"
RESULTS_FILE="${TMPDIR}/parsed-results.json"
FAILURES_FILE="${TMPDIR}/parsed-failures.json"
mkdir -p "${PLUGIN_PATH}/tests" "${PLUGIN_PATH}/config" "${DEP_PATH}/fixtures" "${REMOTE_DEP_PATH}/fixtures" "$ARTIFACTS_DIR" "$WRITABLE_DIR" "$STUBS_DIR"

cat > "${PLUGIN_PATH}/tests/OnlyTest.php" <<'PHP'
<?php
class OnlyTest extends WP_UnitTestCase {}
PHP
printf '<?php // component drop-in\n' > "${PLUGIN_PATH}/db.php"
printf '<?php /*\nPlugin Name: Example\nNetwork: true\n*/\n' > "${PLUGIN_PATH}/example.php"
printf 'component-extra\n' > "${PLUGIN_PATH}/config/component-extra.php"
cat > "${PLUGIN_PATH}/composer.json" <<'JSON'
{"autoload":{"psr-4":{"Example\\":"src/"}}}
JSON
printf '<?php /*\nPlugin Name: Dep Plugin\n*/\n' > "${DEP_PATH}/dep-plugin.php"
printf 'dependency-extra\n' > "${DEP_PATH}/fixtures/dep-extra.php"
printf '<?php /*\nPlugin Name: Dep Plugin Remote Lab Copy\n*/\n' > "${REMOTE_DEP_PATH}/dep-plugin.php"
printf 'remote dependency-extra\n' > "${REMOTE_DEP_PATH}/fixtures/dep-extra.php"

cat > "${STUBS_DIR}/composer" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${COMPOSER_LOG:?}"
mkdir -p vendor
printf '<?php // prepared autoload\n' > vendor/autoload.php
SH
chmod +x "${STUBS_DIR}/composer"

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
#!/usr/bin/env bash
homeboy_resolve_context() {
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="$HOMEBOY_COMPONENT_ID"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
SH
chmod +x "$RESOLVE_CONTEXT_HELPER"

cat > "$WRITE_RESULTS_HELPER" <<'SH'
#!/usr/bin/env bash
homeboy_write_test_results() {
    jq -n --arg source current-runtime --argjson total "$1" --argjson passed "$2" --argjson failed "$3" --argjson skipped "$4" --arg partial "$5" \
        '{source: $source, total: $total, passed: $passed, failed: $failed, skipped: $skipped, partial: $partial}' > "$HOMEBOY_TEST_RESULTS_FILE"
}
SH
chmod +x "$WRITE_RESULTS_HELPER"

cat > "$FAKE_WP_CODEBOX" <<'NODE'
#!/usr/bin/env node
const fs = require('node:fs')
const path = require('node:path')

const args = process.argv.slice(2)
if (args.includes('--version')) { process.stdout.write('0.20.0'); process.exit(0) }
fs.writeFileSync(process.env.FAKE_WP_CODEBOX_ARGS_FILE, `${args.join('\n')}\n`)

if (args[0] === 'recipe' && args[1] === 'build' && args[2] === 'phpunit') {
  const options = JSON.parse(fs.readFileSync(argValue('--options'), 'utf8'))
  const recipe = {
    schema: 'wp-codebox/workspace-recipe/v1',
    runtime: { wp: options.wordpressVersion, blueprint: { steps: [] } },
    inputs: { mounts: options.mounts || [], extra_plugins: options.extra_plugins || [] },
    workflow: {
      steps: [{
        command: 'wordpress.phpunit',
        args: [
          `plugin-slug=${options.pluginSlug}`,
          `test-file=${options.selectedTestFile || ''}`,
          `changed-tests-json=${JSON.stringify(options.changedTestFiles || [])}`,
          `phpunit-args-json=${JSON.stringify(options.phpunitArgs || [])}`,
          `env-json=${JSON.stringify(options.env || {})}`,
          `wp-config-defines-json=${JSON.stringify(options.wpConfigDefines || {})}`,
          `autoload-file=${options.autoloadFile}`,
          `dependency-mounts=${(options.dependencyMounts || []).filter(Boolean).join(',')}`,
          `multisite=${options.multisite ? '1' : '0'}`,
        ],
      }],
    },
  }
  fs.writeFileSync(argValue('--output'), `${JSON.stringify(recipe, null, 2)}\n`)
  process.exit(0)
}

function requirePair(name, value) {
  for (let index = 0; index < args.length - 1; index += 1) {
    if (args[index] === name && args[index + 1] === value) {
      return
    }
  }
  throw new Error(`missing ${name} ${value}`)
}

function argValue(name) {
  const index = args.indexOf(name)
  return index >= 0 ? args[index + 1] : ''
}

for (const expected of ['recipe-run', '--json']) {
  if (!args.includes(expected)) {
    throw new Error(`missing expected wp-codebox arg: ${expected}`)
  }
}

const recipePath = argValue('--recipe')
if (!recipePath) {
  throw new Error('missing --recipe')
}

const recipe = JSON.parse(fs.readFileSync(recipePath, 'utf8'))
if (recipe.runtime?.wp !== '6.10') {
  throw new Error(`unexpected recipe runtime wp: ${recipe.runtime?.wp}`)
}
const step = recipe.workflow?.steps?.[0]
if (step?.command !== 'wordpress.phpunit') {
  throw new Error(`unexpected recipe command: ${step?.command}`)
}

const mounts = recipe.inputs?.mounts || []
for (const expected of [
  'plugin-slug=example',
  'test-file=tests/OnlyTest.php',
  // The changed-file scope is sent sandbox-absolute: WP Codebox normalizes it
  // against the PHPUnit test root, so a component-relative path never matches a
  // discovered file. See Extra-Chill/homeboy#12023.
  'changed-tests-json=["/wordpress/wp-content/plugins/example/tests/OnlyTest.php"]',
  'phpunit-args-json=["tests/OnlyTest.php","--filter","OnlyTest"]',
  'env-json={"HOMEBOY_FLAG":"yes"}',
  'wp-config-defines-json={"WP_DEBUG":true,"CUSTOM_NUMBER":7}',
  'autoload-file=/wp-codebox-vendor/autoload.php',
  'dependency-mounts=/wordpress/wp-content/plugins/example,/wordpress/wp-content/plugins/dep-plugin',
  'multisite=1',
]) {
  if (!(step.args || []).includes(expected)) {
    throw new Error(`step args missing expected value: ${expected}\nactual:\n${(step.args || []).join('\n')}`)
  }
}

const mountStrings = mounts.map((mount) => `${mount.source}:${mount.target}${mount.mode === 'readonly' ? ':readonly' : ''}`)

const requiredMounts = JSON.parse(process.env.REQUIRED_MOUNTS_JSON)
for (const mount of requiredMounts) {
  if (!mountStrings.includes(mount)) {
    throw new Error(`missing mount: ${mount}\nactual:\n${mountStrings.join('\n')}`)
  }
}

const pluginSources = (recipe.inputs?.extra_plugins || []).map((plugin) => plugin.source)
const requiredPluginSources = JSON.parse(process.env.REQUIRED_PLUGIN_SOURCES_JSON)
if (JSON.stringify(pluginSources) !== JSON.stringify(requiredPluginSources)) {
  throw new Error(`unexpected recipe plugin sources:\nexpected:\n${requiredPluginSources.join('\n')}\nactual:\n${pluginSources.join('\n')}`)
}
// The recipe may declare that a validation dependency needs Composer
// preparation, but the component under review never carries that instruction:
// its vendor/ comes from the declared dependency-materialization phase.
const target = (recipe.inputs?.extra_plugins || []).at(-1)
if (target?.composer !== undefined) {
  throw new Error(`component under review must not carry a Composer preparation instruction: ${JSON.stringify(target)}`)
}
if ((recipe.inputs?.extra_plugins || []).slice(0, -1).some((plugin) => plugin.composer !== 'install')) {
  throw new Error('validation dependencies must declare Composer preparation for the substrate to own')
}

const artifactRoot = argValue('--artifacts') || path.join(path.dirname(process.env.FAKE_WP_CODEBOX_ARGS_FILE), 'artifacts')
const runtimeDirectory = 'runtime-fixture'
const phpunitArtifacts = path.join(artifactRoot, runtimeDirectory, 'files', 'phpunit')
fs.mkdirSync(phpunitArtifacts, { recursive: true })
const resultModePath = path.join(path.dirname(artifactRoot), 'runtime-smoke-result-mode')
const resultMode = fs.existsSync(resultModePath) ? fs.readFileSync(resultModePath, 'utf8').trim() : 'passed'
if (resultMode === 'fail-before-diagnostic') {
  process.stderr.write('fixture runtime failed before persisting diagnostics\n')
  process.exit(1)
}
const phpunitResult = resultMode === 'bootstrap-failure'
  ? ['STAGE_FAIL:bootstrap:fixture bootstrap failure', ''].join('\n')
  : ['STAGE_BEGIN:run_tests', 'ALL TESTS PASSED', 'TESTS: 1 FAILURES: 0 ERRORS: 0', 'STAGE_OK:run_tests', ''].join('\n')
// WP Codebox persists the sandbox VFS result to this structured artifact path.
fs.writeFileSync(path.join(phpunitArtifacts, '.pg-test-result.txt'), phpunitResult)
fs.writeFileSync(resultModePath, resultMode === 'passed' ? 'bootstrap-failure\n' : 'passed\n')
const pointerRuntimeDirectory = resultMode === 'invalid-pointer' ? 'runtime-/../../other-run' : runtimeDirectory
fs.writeFileSync(path.join(artifactRoot, 'latest-runtime.json'), JSON.stringify({ paths: { runtimeDirectory: pointerRuntimeDirectory } }))
const filesRoot = path.join(artifactRoot, runtimeDirectory, 'files')
fs.mkdirSync(filesRoot, { recursive: true })
fs.writeFileSync(path.join(filesRoot, 'test-results.json'), JSON.stringify({
  schema: 'wp-codebox/test-results/v1',
  status: 'passed',
  summary: { total: 1, passed: 1, failed: 0, skipped: 0 },
}, null, 2))

process.stdout.write(JSON.stringify({
  success: true,
  executions: [
    {
      stdout: 'OK (1 test, 1 assertion)\n',
      stderr: '',
    },
  ],
  artifacts: {
    directory: path.dirname(filesRoot),
    testResultsPath: path.join(filesRoot, 'test-results.json'),
  },
}) + '\n')
NODE
chmod +x "$FAKE_WP_CODEBOX"


SETTINGS_JSON=$(jq -nc \
    --argjson wpConfig '{"WP_DEBUG":true,"CUSTOM_NUMBER":7}' \
    --argjson benchEnv '{"HOMEBOY_FLAG":"yes"}' \
    --arg writable "$WRITABLE_DIR" \
    --arg runtimeBin "$FAKE_WP_CODEBOX" \
    '{
        runtime_bin: $runtimeBin,
        wordpress_runtime_version: "6.10",
        wp_config_defines: $wpConfig,
        bench_env: $benchEnv,
        wp_codebox_phpunit_mounts: [
            {source: $writable, target: "/tmp/writable-workspace", mode: "readwrite"}
        ]
    }')

REQUIRED_MOUNTS_JSON=$(jq -nc \
    --arg writableWorkspace "${WRITABLE_DIR}:/tmp/writable-workspace" \
    --arg vendor "${EXTENSION_PATH}/vendor:/wp-codebox-vendor:readonly" \
    '[$writableWorkspace, $vendor]')
export REQUIRED_MOUNTS_JSON

# Declared settings mounts and the PHPUnit harness are what this runner still
# translates. The component and its dependencies are carried as extra_plugins,
# and Lab offload must have rewritten the dependency's local workspace path to
# its remote path before the recipe is built.
REQUIRED_PLUGIN_SOURCES_JSON=$(jq -nc \
    --arg component "$PLUGIN_PATH" \
    --arg dep "$REMOTE_DEP_PATH" \
    '[$dep, $component]')
export REQUIRED_PLUGIN_SOURCES_JSON

LAB_OFFLOAD_JSON=$(jq -nc \
    --arg depLocal "$DEP_PATH" \
    --arg depRemote "$REMOTE_DEP_PATH" \
    '{workspace_mappings: [{local_path: $depLocal, remote_path: $depRemote}]}')

bash -n "$SCRIPT_DIR/test-runner-wp-codebox.sh"

run_runner() {
    FAKE_WP_CODEBOX_ARGS_FILE="$ARGS_FILE" \
        COMPOSER_LOG="$COMPOSER_LOG" \
        PATH="${STUBS_DIR}:${PATH}" \
        HOMEBOY_WP_CODEBOX_BIN="$FAKE_WP_CODEBOX" \
        HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
        HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
        HOMEBOY_COMPONENT_ID="example" \
        COMPONENT_ID="example" \
        HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
        HOMEBOY_RUNTIME_RUNNER_STEPS="${TMPDIR}/missing-runner-steps.sh" \
        HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$WRITE_RESULTS_HELPER" \
        HOMEBOY_TEST_RESULTS_FILE="$RESULTS_FILE" \
        HOMEBOY_TEST_FAILURES_FILE="$FAILURES_FILE" \
        HOMEBOY_WORDPRESS_DEPENDENCY_PATHS="$DEP_PATH" \
        HOMEBOY_LAB_OFFLOAD_JSON="$LAB_OFFLOAD_JSON" \
        HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
        HOMEBOY_CHANGED_TEST_FILES="tests/OnlyTest.php" \
        HOMEBOY_WORDPRESS_PHPUNIT_TEST_FILE="tests/OnlyTest.php" \
        HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="$ARTIFACTS_DIR" \
        bash "$RUNNER" tests/OnlyTest.php --filter OnlyTest
}

output=$(run_runner 2>&1)

if [[ "$output" != *"WP Codebox test run complete."* ]]; then
    echo "Expected WP Codebox runner success output" >&2
    echo "$output" >&2
    exit 1
fi

if [ ! -s "$ARGS_FILE" ]; then
    echo "Expected fake wp-codebox to capture arguments" >&2
    exit 1
fi
if ! grep -q '"source": "current-runtime"' "$RESULTS_FILE"; then
    echo "Expected current runtime test-results sidecar to be parsed" >&2
    exit 1
fi
if [ ! -f "$FAILURES_FILE" ] || grep -q 'stale-caller-sidecar' "$FAILURES_FILE"; then
    echo "Expected current runtime failure sidecar to be parsed" >&2
    exit 1
fi

# The runner must not perform dependency materialization. WP Codebox prepares
# Composer autoload for every recipe plugin itself, so a `composer` invocation
# from this runner is a layering regression.
if [ -s "$COMPOSER_LOG" ]; then
    echo "WP Codebox runner must not invoke composer; dependency materialization is not its concern" >&2
    cat "$COMPOSER_LOG" >&2 || true
    exit 1
fi

if [ -e "${PLUGIN_PATH}/.pg-test-result.txt" ]; then
    echo "WP Codebox diagnostics must not write to the readonly component source" >&2
    exit 1
fi
run_artifacts_dir=""
for candidate in "${ARTIFACTS_DIR}"/wp-codebox-phpunit.*; do
    [ -d "$candidate" ] && run_artifacts_dir="$candidate"
done
if ! grep -q '^STAGE_OK:run_tests' "${run_artifacts_dir}/runtime-fixture/files/phpunit/.pg-test-result.txt"; then
    echo "Expected PHPUnit VFS diagnostic to persist under WP Codebox artifacts" >&2
    exit 1
fi

set +e
failure_output=$(run_runner 2>&1)
failure_status=$?
set -e
if [ "$failure_status" -eq 0 ]; then
    echo "Expected structured PHPUnit bootstrap diagnostic to fail the runner" >&2
    exit 1
fi
if [[ "$failure_output" != *"BOOTSTRAP FAILURE: bootstrap:fixture bootstrap failure"* ]]; then
    echo "Expected bootstrap classification from the structured PHPUnit artifact" >&2
    echo "$failure_output" >&2
    exit 1
fi
if [ -e "${PLUGIN_PATH}/.pg-test-result.txt" ]; then
    echo "Structured diagnostic failure must not write to the readonly component source" >&2
    exit 1
fi

# Runtime-artifact pointer handling (crashed runtime, malformed pointer, stale
# caller sidecars) is owned by tests/wp-codebox-phpunit-aggregate-smoke.mjs,
# which asserts the current contract: preserve and parse the captured aggregate
# at the stable artifact root rather than report a zero-test run. Those
# scenarios previously lived here asserting the pre-hardening behaviour.

echo "WP Codebox mount translation smoke passed"
