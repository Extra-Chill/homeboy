#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/test/test-runner-wp-codebox.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

PLUGIN_DIR="${TMPDIR}/sample-plugin"
ARTIFACTS_DIR="${TMPDIR}/artifacts"
FAKE_BIN="${TMPDIR}/wp-codebox"
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context-helper.sh"
EXTRA_MOUNT="${TMPDIR}/extra.php"

mkdir -p "${PLUGIN_DIR}/tests" "$ARTIFACTS_DIR"
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
printf '%s\n' '<?php // extra mount' > "$EXTRA_MOUNT"

cat > "$FAKE_BIN" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [ "${1:-}" = "--version" ]; then
	printf '0.21.0\n'
	exit 0
fi

if [ "${1:-}" = "runtime" ] && [ "${2:-}" = "descriptor" ] && [ "${3:-}" = "--json" ]; then
	printf '%s\n' '{"schema":"wp-codebox/runtime-descriptor/v1","readiness":{"status":"available","browserRuntime":{"status":"ready"}},"contractManifest":{"schemas":{"runtimeBoundary":{"browserContainedSiteOpen":"wp-codebox/browser-contained-site-open/v1"}}}}'
	exit 0
fi

if [ "${1:-}" = "commands" ]; then
	exit 0
fi

if [ "${1:-}" = "recipe" ] && [ "${2:-}" = "build" ]; then
	shift 3
	while [ "$#" -gt 0 ]; do
		if [ "$1" = "--output" ]; then
			printf '%s\n' '{"schema":"wp-codebox/workspace-recipe/v1"}' > "$2"
			exit 0
		fi
		shift
	done
	exit 2
fi

recipe=""
artifacts=""
while [ "$#" -gt 0 ]; do
	case "$1" in
		--recipe)
			shift
			recipe="${1:-}"
			;;
		--artifacts)
			shift
			artifacts="${1:-}"
			;;
	esac
	shift || true
done

[ -n "$recipe" ] || { echo "missing --recipe" >&2; exit 2; }
[ -n "$artifacts" ] || { echo "missing --artifacts" >&2; exit 2; }
mkdir -p "$artifacts"
printf '%s\n' '{"executions":[{"stdout":"FAILURES!\nTests: 1, Assertions: 1, Failures: 1."}]}'
exit 1
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

set +e
HOMEBOY_COMPONENT_ID="sample-plugin" \
HOMEBOY_COMPONENT_PATH="$PLUGIN_DIR" \
HOMEBOY_PROJECT_PATH="$PLUGIN_DIR" \
HOMEBOY_EXTENSION_PATH="$ROOT_DIR" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_RUNNER_STEPS="${TMPDIR}/missing-runner-steps.sh" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="$ARTIFACTS_DIR" \
HOMEBOY_SETTINGS_JSON='{"wp_codebox_phpunit_test_root":"tests","wp_codebox_phpunit_config":"phpunit.xml.dist","wp_codebox_phpunit_cwd":"tests","wp_codebox_phpunit_bootstrap_mode":"managed","wp_codebox_phpunit_mounts":[{"source":"'"$EXTRA_MOUNT"'","target":"/tmp/extra.php","mode":"readonly"}]}' \
    bash "$RUNNER" --filter SampleFilter > "${TMPDIR}/runner.out" 2>&1
status=$?
set -e

if [ "$status" -eq 0 ]; then
	echo "Expected WP Codebox PHPUnit runner to fail" >&2
	exit 1
fi

node - "$ARTIFACTS_DIR" "$FAKE_BIN" <<'NODE'
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const artifactsDir = process.argv[2];
const fakeBin = process.argv[3];

const runDirectory = fs.readdirSync(artifactsDir).find((entry) => entry.startsWith('wp-codebox-phpunit.'));
assert.ok(runDirectory, 'expected a PHPUnit artifact directory');
const readJson = (name) => JSON.parse(fs.readFileSync(path.join(artifactsDir, runDirectory, name), 'utf8'));
const recipe = readJson('wp-codebox-phpunit-recipe.json');
const options = readJson('wp-codebox-phpunit-recipe-options.json');
const provenance = readJson('wp-codebox-phpunit-provenance.json');
const profile = readJson('wp-codebox-phpunit-profile.json');

assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
assert.equal(options.pluginSlug, 'sample-plugin');
assert.equal(options.testRoot, 'tests');
assert.equal(options.phpunitXml, 'phpunit.xml.dist');
assert.equal(options.cwd, 'tests');
assert.deepEqual(options.phpunitArgs, ['--filter', 'SampleFilter']);
assert.equal(provenance.wp_codebox.cli_bin, fakeBin);
assert.equal(provenance.wp_codebox.resolved_cli_path, fakeBin);
assert.deepEqual(provenance.wp_codebox.command, [fakeBin]);
assert.equal(profile.phpunit.test_root, 'tests');
assert.equal(profile.phpunit.config, 'phpunit.xml.dist');
assert.equal(profile.phpunit.cwd, 'tests');
assert.equal(profile.phpunit.bootstrap_mode, 'managed');
assert.deepEqual(profile.phpunit.passthrough_args, ['--filter', 'SampleFilter']);
assert.equal(profile.phpunit.extra_mounts[0].target, '/tmp/extra.php');
NODE

echo "WP Codebox PHPUnit failure artifacts smoke passed"
