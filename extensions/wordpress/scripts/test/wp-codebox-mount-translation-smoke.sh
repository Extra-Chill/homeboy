#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner.sh"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

PLUGIN_PATH="${TMPDIR}/component"
DEP_PATH="${TMPDIR}/dep-plugin@branch"
FAKE_WP_CODEBOX="${TMPDIR}/wp-codebox.js"
ARGS_FILE="${TMPDIR}/wp-codebox-args.txt"
ARTIFACTS_DIR="${TMPDIR}/artifacts"
mkdir -p "${PLUGIN_PATH}/tests" "${PLUGIN_PATH}/config" "${DEP_PATH}/fixtures" "$ARTIFACTS_DIR"

cat > "${PLUGIN_PATH}/tests/OnlyTest.php" <<'PHP'
<?php
class OnlyTest extends WP_UnitTestCase {}
PHP
printf '<?php // component drop-in\n' > "${PLUGIN_PATH}/db.php"
printf 'component-extra\n' > "${PLUGIN_PATH}/config/component-extra.php"
printf '<?php /*\nPlugin Name: Dep Plugin\n*/\n' > "${DEP_PATH}/dep-plugin.php"
printf 'dependency-extra\n' > "${DEP_PATH}/fixtures/dep-extra.php"

cat > "$FAKE_WP_CODEBOX" <<'NODE'
#!/usr/bin/env node
const fs = require('node:fs')
const path = require('node:path')

const args = process.argv.slice(2)
fs.writeFileSync(process.env.FAKE_WP_CODEBOX_ARGS_FILE, `${args.join('\n')}\n`)

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
  'changed-tests-json=["tests/OnlyTest.php"]',
  'env-json={"HOMEBOY_FLAG":"yes"}',
  'wp-config-defines-json={"WP_DEBUG":true,"CUSTOM_NUMBER":7}',
  'autoload-file=/wp-codebox-vendor/autoload.php',
  'tests-dir=/wp-codebox-vendor/wp-phpunit/wp-phpunit',
  'dependency-mounts=/wordpress/wp-content/plugins/dep-plugin',
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

const componentMount = mountStrings.find((mount) => mount.endsWith(':/wordpress/wp-content/plugins/example'))
if (!componentMount) {
  throw new Error('component mount missing')
}
const componentPath = componentMount.slice(0, -':/wordpress/wp-content/plugins/example'.length)
fs.writeFileSync(path.join(componentPath, '.pg-test-result.txt'), [
  'STAGE_BEGIN:run_tests',
  'ALL TESTS PASSED',
  'TESTS: 1 FAILURES: 0 ERRORS: 0',
  'STAGE_OK:run_tests',
  '',
].join('\n'))

const artifactRoot = argValue('--artifacts') || path.join(path.dirname(process.env.FAKE_WP_CODEBOX_ARGS_FILE), 'artifacts')
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
    '{
        playground_wordpress_version: "6.10",
        wp_config_defines: $wpConfig,
        bench_env: $benchEnv,
        playground_file_mounts: [
            {from: "config/component-extra.php", to: "/wordpress/wp-content/component-extra.php"},
            {from_dependency: "dep-plugin", from: "fixtures/dep-extra.php", to: "/wordpress/wp-content/dep-extra.php"}
        ]
    }')

REQUIRED_MOUNTS_JSON=$(jq -nc \
    --arg component "${PLUGIN_PATH}:/wordpress/wp-content/plugins/example" \
    --arg dep "${DEP_PATH}:/wordpress/wp-content/plugins/dep-plugin" \
    --arg dropin "${PLUGIN_PATH}/db.php:/wordpress/wp-content/db.php" \
    --arg componentExtra "${PLUGIN_PATH}/config/component-extra.php:/wordpress/wp-content/component-extra.php" \
    --arg depExtra "${DEP_PATH}/fixtures/dep-extra.php:/wordpress/wp-content/dep-extra.php" \
    --arg vendor "${EXTENSION_PATH}/vendor:/wp-codebox-vendor:readonly" \
    '[$component, $dep, $dropin, $componentExtra, $depExtra, $vendor]')
export REQUIRED_MOUNTS_JSON

bash -n "$SCRIPT_DIR/test-runner-wp-codebox.sh"

output=$(FAKE_WP_CODEBOX_ARGS_FILE="$ARGS_FILE" \
    HOMEBOY_WP_CODEBOX_BIN="$FAKE_WP_CODEBOX" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPONENT_ID="example" \
    HOMEBOY_WORDPRESS_DEPENDENCY_PATHS="$DEP_PATH" \
    HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    HOMEBOY_CHANGED_TEST_FILES="tests/OnlyTest.php" \
    HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="$ARTIFACTS_DIR" \
    bash "$RUNNER" tests/OnlyTest.php --filter OnlyTest 2>&1)

if [[ "$output" != *"WP Codebox test run complete."* ]]; then
    echo "Expected WP Codebox runner success output" >&2
    echo "$output" >&2
    exit 1
fi

if [ ! -s "$ARGS_FILE" ]; then
    echo "Expected fake wp-codebox to capture arguments" >&2
    exit 1
fi

echo "WP Codebox mount translation smoke passed"
