#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/test/test-runner-wp-codebox.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

MONOREPO_DIR="${TMPDIR}/monorepo"
PLUGIN_DIR="${MONOREPO_DIR}/plugins/sample-plugin"
FAKE_BIN="${TMPDIR}/wp-codebox"
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context-helper.sh"
CAPTURED_RECIPE="${TMPDIR}/recipe.json"
CORE_MODULE="${ROOT_DIR}/tests/fixtures/wp-codebox-core-recipe-builder.mjs"

mkdir -p "${PLUGIN_DIR}/tests"
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

cat > "$FAKE_BIN" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

recipe=""
while [ "$#" -gt 0 ]; do
	case "$1" in
		--recipe)
			shift
			recipe="${1:-}"
			;;
	esac
	shift || true
done

if [ -z "$recipe" ]; then
	echo "missing --recipe" >&2
	exit 2
fi

cp "$recipe" "$CAPTURED_RECIPE"
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
HOMEBOY_EXTENSION_PATH="$ROOT_DIR" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_RUNNER_STEPS="${TMPDIR}/missing-runner-steps.sh" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
HOMEBOY_WP_CODEBOX_CORE_MODULE="$CORE_MODULE" \
HOMEBOY_SETTINGS_WP_CODEBOX_SOURCE_ROOT="$MONOREPO_DIR" \
HOMEBOY_SETTINGS_WP_CODEBOX_SOURCE_SUBPATH="plugins/sample-plugin" \
CAPTURED_RECIPE="$CAPTURED_RECIPE" \
HOMEBOY_SETTINGS_JSON="{\"phpunit_no_tests\":\"skip\"}" \
bash "$RUNNER" >/dev/null

node - "$CAPTURED_RECIPE" "$MONOREPO_DIR" <<'NODE'
const assert = require('node:assert/strict');
const fs = require('node:fs');

const recipe = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const monorepo = process.argv[3];

assert.deepEqual(recipe.inputs.extra_plugins, [{
	source: monorepo,
	sourceRoot: monorepo,
	sourceSubpath: 'plugins/sample-plugin',
	slug: 'sample-plugin',
	activate: false,
}]);
NODE

echo "WP Codebox PHPUnit source-root recipe smoke passed"
