#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner-wp-codebox.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

PLUGIN_DIR="${TMPDIR}/sample-plugin"
FAKE_BIN="${TMPDIR}/wp-codebox"
FAKE_BUILDER="${TMPDIR}/build-recipe.mjs"
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context-helper.sh"
CAPTURED_CORE_MODULE="${TMPDIR}/captured-core-module.txt"
CONFIGURED_CORE_MODULE="${TMPDIR}/runtime-core/dist/index.js"
DEFAULT_CORE_MODULE="${TMPDIR}/default-runtime-core/dist/index.js"

mkdir -p "$PLUGIN_DIR/tests" "$(dirname "$CONFIGURED_CORE_MODULE")" "$(dirname "$DEFAULT_CORE_MODULE")"
touch "$CONFIGURED_CORE_MODULE"
touch "$DEFAULT_CORE_MODULE"
cat > "${PLUGIN_DIR}/sample-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Sample Plugin
 */
PHP
cat > "${PLUGIN_DIR}/tests/SampleTest.php" <<'PHP'
<?php

class SampleTest extends WP_UnitTestCase {
	public function test_sample(): void {
		$this->assertTrue( true );
	}
}
PHP

cat > "$FAKE_BUILDER" <<'JS'
import fs from 'node:fs';

fs.readFileSync(0, 'utf8');
process.stdout.write(JSON.stringify({
  schema: 'wp-codebox/workspace-recipe/v1',
  inputs: { mounts: [] },
  workflow: { steps: [{ command: 'wordpress.phpunit', args: ['plugin-slug=sample-plugin'] }] },
}));
JS

cat > "$FAKE_BIN" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "commands" ]; then
    printf '%s\n' 'recipe-run'
    exit 0
fi

printf '%s' "${HOMEBOY_WP_CODEBOX_CORE_MODULE:-}" > "$CAPTURED_CORE_MODULE"
printf '%s\n' 'NO_TEST_FILES' > "${HOMEBOY_PLUGIN_PATH}/.pg-test-result.txt"
printf '%s\n' '{"executions":[{"stdout":"NO_TEST_FILES"}]}'
SH
chmod +x "$FAKE_BIN"

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
#!/usr/bin/env bash
homeboy_resolve_context() {
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="$HOMEBOY_COMPONENT_ID"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
SH
chmod +x "$RESOLVE_CONTEXT_HELPER"

HOMEBOY_COMPONENT_ID="sample-plugin" \
HOMEBOY_COMPONENT_PATH="$PLUGIN_DIR" \
HOMEBOY_PROJECT_PATH="$PLUGIN_DIR" \
HOMEBOY_EXTENSION_PATH="${SCRIPT_DIR}/.." \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_RUNNER_STEPS="${TMPDIR}/missing-runner-steps.sh" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER="$FAKE_BUILDER" \
HOMEBOY_WP_CODEBOX_CORE_MODULE="$DEFAULT_CORE_MODULE" \
CAPTURED_CORE_MODULE="$CAPTURED_CORE_MODULE" \
HOMEBOY_SETTINGS_JSON="{\"wp_codebox_core_module\":\"$CONFIGURED_CORE_MODULE\",\"phpunit_no_tests\":\"skip\"}" \
bash "$RUNNER" >/dev/null

captured="$(cat "$CAPTURED_CORE_MODULE")"
if [ "$captured" != "$CONFIGURED_CORE_MODULE" ]; then
    echo "Expected test runner to export wp_codebox_core_module from settings" >&2
    echo "Expected: $CONFIGURED_CORE_MODULE" >&2
    echo "Actual:   $captured" >&2
    exit 1
fi

echo "WP Codebox test core module setting smoke passed"
