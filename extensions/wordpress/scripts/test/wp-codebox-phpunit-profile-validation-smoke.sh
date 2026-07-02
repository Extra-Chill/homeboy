#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner-wp-codebox.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

PLUGIN_DIR="${TMPDIR}/sample-plugin"
EXTRA_MOUNT="${TMPDIR}/extra-config"
FAKE_BIN="${TMPDIR}/wp-codebox"
FAKE_BUILDER="${TMPDIR}/build-recipe.mjs"
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context-helper.sh"
WP_CODEBOX_RAN_FILE="${TMPDIR}/wp-codebox-ran.txt"

mkdir -p "$PLUGIN_DIR/tests" "$EXTRA_MOUNT"
printf '<?php /*\nPlugin Name: Sample Plugin\n*/\n' > "${PLUGIN_DIR}/sample-plugin.php"
printf '<?php class SampleTest extends WP_UnitTestCase {}\n' > "${PLUGIN_DIR}/tests/SampleTest.php"
printf 'custom config\n' > "${EXTRA_MOUNT}/phpunit.ini"

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
#!/usr/bin/env bash
homeboy_resolve_context() {
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="$HOMEBOY_COMPONENT_ID"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
SH
chmod +x "$RESOLVE_CONTEXT_HELPER"

cat > "$FAKE_BUILDER" <<'JS'
import fs from 'node:fs';

const options = JSON.parse(fs.readFileSync(0, 'utf8'));
const omitProfile = process.env.OMIT_PHPUNIT_PROFILE === '1';
const recipe = {
  schema: 'wp-codebox/workspace-recipe/v1',
  inputs: {
    mounts: omitProfile ? [] : (options.mounts || []),
  },
  workflow: {
    steps: [{
      command: 'wordpress.phpunit',
      args: [
        `plugin-slug=${options.pluginSlug}`,
        ...(omitProfile ? [] : [
          `cwd=${options.cwd || ''}`,
          `test-root=${options.testRoot || ''}`,
          `phpunit-xml=${options.phpunitXml || ''}`,
        ]),
      ],
    }],
  },
};

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);
JS

cat > "$FAKE_BIN" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "commands" ]; then
    printf '%s\n' 'recipe-run'
    exit 0
fi
printf 'ran\n' > "${WP_CODEBOX_RAN_FILE:?}"
printf 'NO_TEST_FILES\n' > "${HOMEBOY_PLUGIN_PATH}/.pg-test-result.txt"
printf '%s\n' '{"executions":[{"stdout":"NO_TEST_FILES"}]}'
SH
chmod +x "$FAKE_BIN"

settings_json=$(jq -nc \
    --arg mountSource "$EXTRA_MOUNT" \
    '{
        phpunit_no_tests: "skip",
        wp_codebox_phpunit_mounts: [{source: $mountSource, target: "/wordpress/custom/phpunit", mode: "readonly"}],
        wp_codebox_phpunit_cwd: "/wordpress/custom/cwd",
        wp_codebox_phpunit_test_root: "/wordpress/custom/tests/phpunit",
        wp_codebox_phpunit_config: "/wordpress/custom/phpunit.xml.dist"
    }')

run_runner() {
    HOMEBOY_COMPONENT_ID="sample-plugin" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_DIR" \
    HOMEBOY_PROJECT_PATH="$PLUGIN_DIR" \
    HOMEBOY_EXTENSION_PATH="${SCRIPT_DIR}/../.." \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="${TMPDIR}/missing-runner-steps.sh" \
    HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
    HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER="$FAKE_BUILDER" \
    HOMEBOY_SETTINGS_JSON="$settings_json" \
    WP_CODEBOX_RAN_FILE="$WP_CODEBOX_RAN_FILE" \
    bash "$RUNNER"
}

set +e
valid_output=$(run_runner 2>&1)
valid_status=$?
set -e
if [ "$valid_status" -ne 0 ]; then
    echo "Expected valid profile recipe to pass" >&2
    echo "$valid_output" >&2
    exit 1
fi
if [ ! -f "$WP_CODEBOX_RAN_FILE" ]; then
    echo "Expected WP Codebox to run after valid profile recipe generation" >&2
    echo "$valid_output" >&2
    exit 1
fi

rm -f "$WP_CODEBOX_RAN_FILE"
set +e
output=$(OMIT_PHPUNIT_PROFILE=1 run_runner 2>&1)
status=$?
set -e

if [ "$status" -eq 0 ]; then
    echo "Expected profile validation to fail when generated recipe omits requested settings" >&2
    echo "$output" >&2
    exit 1
fi

if [ -f "$WP_CODEBOX_RAN_FILE" ]; then
    echo "Expected WP Codebox not to run after profile validation failure" >&2
    echo "$output" >&2
    exit 1
fi

for expected in \
    "WP CODEBOX PHPUNIT PROFILE CONFIGURATION FAILURE" \
    "wp_codebox_phpunit_mounts entry missing" \
    "wp_codebox_phpunit_cwd missing" \
    "wp_codebox_phpunit_test_root missing" \
    "wp_codebox_phpunit_config missing"; do
    if [[ "$output" != *"$expected"* ]]; then
        echo "Expected diagnostic containing: $expected" >&2
        echo "$output" >&2
        exit 1
    fi
done

echo "WP Codebox PHPUnit profile validation smoke passed"
