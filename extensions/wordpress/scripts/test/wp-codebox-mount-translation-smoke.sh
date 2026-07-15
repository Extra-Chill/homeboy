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
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context-helper.sh"
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

cat > "$FAKE_WP_CODEBOX" <<'NODE'
#!/usr/bin/env node
const fs = require('node:fs')
const path = require('node:path')

const args = process.argv.slice(2)
fs.writeFileSync(process.env.FAKE_WP_CODEBOX_ARGS_FILE, `${args.join('\n')}\n`)

if (args[0] === 'recipe' && args[1] === 'build' && args[2] === 'phpunit') {
  const options = JSON.parse(fs.readFileSync(argValue('--options'), 'utf8'))
  const recipe = {
    schema: 'wp-codebox/workspace-recipe/v1',
    runtime: { wp: options.wordpressVersion, blueprint: { steps: [] } },
    inputs: { mounts: options.mounts || [] },
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
          `tests-dir=${options.testsDir}`,
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
  'test-file=OnlyTest.php',
  'changed-tests-json=["tests/OnlyTest.php"]',
  'phpunit-args-json=["--filter","OnlyTest","--cache-result-file=/tmp/wp-codebox-phpunit.result.cache"]',
  'env-json={}',
  'wp-config-defines-json={"WP_DEBUG":true,"CUSTOM_NUMBER":7}',
  'autoload-file=/wp-codebox-vendor/autoload.php',
  'tests-dir=/wp-codebox-vendor/wp-phpunit/wp-phpunit',
  'dependency-mounts=/wordpress/wp-content/plugins/dep-plugin',
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

const componentMountSuffix = ':/wordpress/wp-content/plugins/example:readonly'
const componentMount = mountStrings.find((mount) => mount.endsWith(componentMountSuffix))
if (!componentMount) {
  throw new Error('component mount missing')
}
const componentPath = componentMount.slice(0, -componentMountSuffix.length)
if (!fs.existsSync(path.join(componentPath, 'vendor/autoload.php'))) {
  throw new Error(`component Composer autoload was not prepared before mount: ${componentPath}`)
}

const artifactRoot = argValue('--artifacts') || path.join(path.dirname(process.env.FAKE_WP_CODEBOX_ARGS_FILE), 'artifacts')
const phpunitArtifacts = path.join(artifactRoot, 'files', 'phpunit')
fs.mkdirSync(phpunitArtifacts, { recursive: true })
const resultModePath = path.join(artifactRoot, 'runtime-smoke-result-mode')
const phpunitResult = fs.existsSync(resultModePath)
  ? ['STAGE_FAIL:bootstrap:fixture bootstrap failure', ''].join('\n')
  : ['STAGE_BEGIN:run_tests', 'ALL TESTS PASSED', 'TESTS: 1 FAILURES: 0 ERRORS: 0', 'STAGE_OK:run_tests', ''].join('\n')
// WP Codebox persists the sandbox VFS result to this structured artifact path.
fs.writeFileSync(path.join(phpunitArtifacts, '.pg-test-result.txt'), phpunitResult)
fs.writeFileSync(resultModePath, 'bootstrap-failure\n')
const filesRoot = path.join(artifactRoot, 'runtime-smoke', 'files')
fs.mkdirSync(filesRoot, { recursive: true })
fs.writeFileSync(path.join(filesRoot, 'test-results.json'), JSON.stringify({ schema: 'wp-codebox/test-results/v1', status: 'passed' }, null, 2))

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
        ],
        wp_codebox_file_mounts: [
            {from: "config/component-extra.php", to: "/wordpress/wp-content/component-extra.php"},
            {from_dependency: "dep-plugin", from: "fixtures/dep-extra.php", to: "/wordpress/wp-content/dep-extra.php"}
        ]
    }')

REQUIRED_MOUNTS_JSON=$(jq -nc \
    --arg component "${PLUGIN_PATH}:/wordpress/wp-content/plugins/example:readonly" \
    --arg dep "${REMOTE_DEP_PATH}:/wordpress/wp-content/plugins/dep-plugin:readonly" \
    --arg dropin "${PLUGIN_PATH}/db.php:/wordpress/wp-content/db.php:readonly" \
    --arg componentExtra "${PLUGIN_PATH}/config/component-extra.php:/wordpress/wp-content/component-extra.php:readonly" \
    --arg depExtra "${REMOTE_DEP_PATH}/fixtures/dep-extra.php:/wordpress/wp-content/dep-extra.php:readonly" \
    --arg writableWorkspace "${WRITABLE_DIR}:/tmp/writable-workspace" \
    --arg vendor "${EXTENSION_PATH}/vendor:/wp-codebox-vendor:readonly" \
    --arg extension "${EXTENSION_PATH}:/homeboy-extension:readonly" \
    '[$component, $dep, $dropin, $componentExtra, $depExtra, $writableWorkspace, $vendor, $extension]')
export REQUIRED_MOUNTS_JSON

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
        HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
        HOMEBOY_RUNTIME_RUNNER_STEPS="${TMPDIR}/missing-runner-steps.sh" \
        HOMEBOY_WORDPRESS_DEPENDENCY_PATHS="$DEP_PATH" \
        HOMEBOY_LAB_OFFLOAD_JSON="$LAB_OFFLOAD_JSON" \
        HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
        HOMEBOY_CHANGED_TEST_FILES="tests/OnlyTest.php" \
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

if ! grep -q -- 'install --no-dev --no-interaction --no-progress --prefer-dist' "$COMPOSER_LOG"; then
    echo "Expected WP Codebox runner to prepare missing component Composer autoload" >&2
    cat "$COMPOSER_LOG" >&2 || true
    exit 1
fi

if [ -e "${PLUGIN_PATH}/.pg-test-result.txt" ]; then
    echo "WP Codebox diagnostics must not write to the readonly component source" >&2
    exit 1
fi
if ! grep -q '^STAGE_OK:run_tests' "${ARTIFACTS_DIR}/files/phpunit/.pg-test-result.txt"; then
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

echo "WP Codebox mount translation smoke passed"
