#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner.sh"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

PLUGIN_PATH="${TMPDIR}/fixture-plugin"
FAKE_WP_CODEBOX="${TMPDIR}/wp-codebox.js"
CAPTURED_RECIPE="${TMPDIR}/recipe.json"
mkdir -p "${PLUGIN_PATH}/tests/legacy"

printf '<?php
class FixtureBootstrapModeTest extends WP_UnitTestCase {}
' > "${PLUGIN_PATH}/tests/FixtureBootstrapModeTest.php"
printf '<?php
// Project bootstrap fixture.
' > "${PLUGIN_PATH}/tests/legacy/bootstrap.php"
cat > "${PLUGIN_PATH}/phpunit.xml.dist" <<'XML'
<?xml version="1.0"?>
<phpunit bootstrap="tests/legacy/bootstrap.php">
  <testsuites>
    <testsuite name="fixture">
      <directory>tests</directory>
    </testsuite>
  </testsuites>
</phpunit>
XML

cat > "$FAKE_WP_CODEBOX" <<'NODE'
#!/usr/bin/env node
const fs = require('node:fs');
const recipeIndex = process.argv.indexOf('--recipe');
if (recipeIndex === -1 || !process.argv[recipeIndex + 1]) {
  process.stderr.write('missing --recipe\n');
  process.exit(2);
}
fs.copyFileSync(process.argv[recipeIndex + 1], process.env.CAPTURED_RECIPE);
process.stdout.write(JSON.stringify({
  success: true,
  executions: [{ stdout: 'PHPUnit 9.6.34 by Sebastian Bergmann and contributors.\n\nOK (1 test, 1 assertion)\n', stderr: '' }],
}) + '\n');
NODE
chmod +x "$FAKE_WP_CODEBOX"

run_case() {
    local settings_json="$1"
    shift

    rm -f "$CAPTURED_RECIPE"
    HOMEBOY_WP_CODEBOX_BIN="$FAKE_WP_CODEBOX" \
        HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
        HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
        HOMEBOY_COMPONENT_ID="fixture-plugin" \
        HOMEBOY_COMPONENT_SHAPE="plugin" \
        HOMEBOY_SETTINGS_JSON="$settings_json" \
        CAPTURED_RECIPE="$CAPTURED_RECIPE" \
        bash "$RUNNER" "$@" >/dev/null

    if [ ! -f "$CAPTURED_RECIPE" ]; then
        echo "Expected fake WP Codebox to capture a recipe" >&2
        exit 1
    fi
}

assert_recipe_args() {
    local expected_mode="$1"
    local expected_project_bootstrap="$2"
    local expected_filter="$3"
    local expected_test_file="$4"

    node - "$CAPTURED_RECIPE" "$expected_mode" "$expected_project_bootstrap" "$expected_filter" "$expected_test_file" <<'NODE'
const assert = require('node:assert/strict');
const fs = require('node:fs');

const [, , recipePath, expectedMode, expectedProjectBootstrap, expectedFilter, expectedTestFile] = process.argv;
const recipe = JSON.parse(fs.readFileSync(recipePath, 'utf8'));
const args = recipe.workflow.steps[0].args;

assert(args.includes(`bootstrap-mode=${expectedMode}`), args.join('\n'));
assert(args.includes(`project-bootstrap=${expectedProjectBootstrap}`), args.join('\n'));
assert(args.includes(`test-file=${expectedTestFile}`), args.join('\n'));
assert(args.includes(`phpunit-args-json=["--filter","${expectedFilter}"]`), args.join('\n'));
NODE
}

run_case '{}' --filter FixtureBootstrapModeTest::test_auto tests/FixtureBootstrapModeTest.php
assert_recipe_args project tests/legacy/bootstrap.php FixtureBootstrapModeTest::test_auto tests/FixtureBootstrapModeTest.php

run_case '{"wp_codebox_phpunit_bootstrap_mode":"managed"}' --filter FixtureBootstrapModeTest::test_managed tests/FixtureBootstrapModeTest.php
assert_recipe_args managed '' FixtureBootstrapModeTest::test_managed tests/FixtureBootstrapModeTest.php

run_case '{"wp_codebox_phpunit_bootstrap_mode":"project","wp_codebox_phpunit_project_bootstrap":"tests/legacy/bootstrap.php"}' --filter FixtureBootstrapModeTest::test_project tests/FixtureBootstrapModeTest.php
assert_recipe_args project tests/legacy/bootstrap.php FixtureBootstrapModeTest::test_project tests/FixtureBootstrapModeTest.php

echo "WP Codebox project bootstrap mode smoke passed"
