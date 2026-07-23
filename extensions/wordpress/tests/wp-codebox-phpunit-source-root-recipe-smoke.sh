#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/test/test-runner-wp-codebox.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

MONOREPO_DIR="${TMPDIR}/monorepo"
PLUGIN_DIR="${MONOREPO_DIR}/plugins/sample-plugin"
DEPENDENCY_DIR="${TMPDIR}/sample-dependency"
FAKE_BIN="${TMPDIR}/wp-codebox"
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context-helper.sh"
CAPTURED_RECIPE="${TMPDIR}/recipe.json"
RUNNER_OUTPUT="${TMPDIR}/runner-output.txt"

mkdir -p "${PLUGIN_DIR}/tests" "$DEPENDENCY_DIR"
cat > "${PLUGIN_DIR}/sample-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Sample Plugin
 */
PHP
cat > "${DEPENDENCY_DIR}/sample-dependency.php" <<'PHP'
<?php
/**
 * Plugin Name: Sample Dependency
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

cat > "$FAKE_BIN" <<'JS'
#!/usr/bin/env node
const fs = require('node:fs');

const args = process.argv.slice(2);
const option = (name) => args[args.indexOf(name) + 1];

if (args[0] === 'recipe' && args[1] === 'build' && args[2] === 'phpunit') {
	const options = JSON.parse(fs.readFileSync(option('--options'), 'utf8'));
	const extraPlugins = (options.extra_plugins || []).map((plugin) => ({
		...plugin,
		sourceRoot: plugin.source,
	}));
	fs.writeFileSync(option('--output'), JSON.stringify({
		schema: 'wp-codebox/workspace-recipe/v1',
		inputs: { extra_plugins: extraPlugins },
	}));
	process.exit(0);
}

if (args[0] === 'recipe-run') {
	fs.copyFileSync(option('--recipe'), process.env.CAPTURED_RECIPE);
	process.stdout.write('{"executions":[{"stdout":"NO_TEST_FILES"}]}\n');
	process.exit(0);
}

process.exit(2);
JS
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

set +e
HOMEBOY_COMPONENT_ID="sample-plugin" \
HOMEBOY_COMPONENT_PATH="$PLUGIN_DIR" \
HOMEBOY_PROJECT_PATH="$PLUGIN_DIR" \
HOMEBOY_EXTENSION_PATH="$ROOT_DIR" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_RUNNER_STEPS="${TMPDIR}/missing-runner-steps.sh" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
HOMEBOY_WORDPRESS_DEPENDENCY_PATHS="$DEPENDENCY_DIR" \
CAPTURED_RECIPE="$CAPTURED_RECIPE" \
HOMEBOY_SETTINGS_JSON="{\"phpunit_no_tests\":\"skip\",\"wp_codebox_source_root\":\"$MONOREPO_DIR\",\"wp_codebox_source_subpath\":\"plugins/sample-plugin\"}" \
bash "$RUNNER" >"$RUNNER_OUTPUT" 2>&1
runner_status=$?
set -e
if [ "$runner_status" -ne 0 ]; then
    cat "$RUNNER_OUTPUT" >&2
    exit "$runner_status"
fi

node - "$CAPTURED_RECIPE" "$MONOREPO_DIR" "$DEPENDENCY_DIR" <<'NODE'
const assert = require('node:assert/strict');
const fs = require('node:fs');

const recipe = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const monorepo = process.argv[3];
const dependency = process.argv[4];

assert.deepEqual(recipe.inputs.extra_plugins, [
	{
		source: monorepo,
		sourceRoot: monorepo,
		sourceSubpath: 'plugins/sample-plugin',
		slug: 'sample-plugin',
		activate: false,
	},
	{
		source: dependency,
		sourceRoot: dependency,
		slug: 'sample-dependency',
		activate: true,
	},
]);
NODE

echo "WP Codebox PHPUnit source-root recipe smoke passed"
