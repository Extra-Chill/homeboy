'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-primitive-preflight-'));

try {
	const extensionPath = path.join(__dirname, '..');
	const checker = path.join(extensionPath, 'scripts', 'bench', 'check-wp-codebox-bench-primitive.mjs');
	const componentPath = path.join(root, 'primitive-fixture');
	const benchWorkload = path.join(root, 'woocommerce-external-http-guardrail.php');
	const staleRoot = path.join(root, 'stale-wp-codebox');
	const currentRoot = path.join(root, 'current-wp-codebox');

	function writeWpCodeboxFixture(packageRoot, source) {
		const bin = path.join(packageRoot, 'packages', 'cli', 'dist', 'index.js');
		const runtimeSource = path.join(packageRoot, 'packages', 'runtime-playground', 'src', 'bench-command-handlers.ts');
		fs.mkdirSync(path.dirname(bin), { recursive: true });
		fs.mkdirSync(path.dirname(runtimeSource), { recursive: true });
		fs.writeFileSync(path.join(packageRoot, 'package.json'), JSON.stringify({ name: 'wp-codebox-fixture' }, null, 2));
		fs.writeFileSync(bin, '#!/usr/bin/env node\nprocess.exit(99);\n');
		fs.chmodSync(bin, 0o755);
		fs.writeFileSync(runtimeSource, source);
		return bin;
	}

	const staleBin = writeWpCodeboxFixture(staleRoot, 'export function benchRunCode() { return "no primitive"; }\n');
	const currentBin = writeWpCodeboxFixture(currentRoot, [
		'export function benchRunCode() {',
		'  return "function wp_codebox_bench_run_external_http_guardrail_step() {} elseif ($type === \'external-http-guardrail\') {}";',
		'}',
		'',
	].join('\n'));

	const staleCheck = spawnSync(process.execPath, [checker, 'external-http-guardrail', staleBin], { encoding: 'utf8' });
	assert.equal(staleCheck.status, 1);
	assert.match(staleCheck.stderr, /external-http-guardrail bench primitive is not available/);
	assert.match(staleCheck.stderr, /Selected wp-codebox binary:/);

	const currentCheck = spawnSync(process.execPath, [checker, 'external-http-guardrail', currentBin], { encoding: 'utf8' });
	assert.equal(currentCheck.status, 0, currentCheck.stderr);

	fs.mkdirSync(componentPath, { recursive: true });
	fs.writeFileSync(path.join(componentPath, 'primitive-fixture.php'), "<?php\n/* Plugin Name: Primitive Fixture */\n");
	fs.writeFileSync(benchWorkload, "<?php\nreturn static fn() => array( 'metrics' => array( 'noop' => 1 ) );\n");

	const benchHelper = path.join(root, 'bench-helper.sh');
	fs.writeFileSync(benchHelper, `#!/usr/bin/env bash
homeboy_write_empty_bench_results() {
  local component="$1"
  local iterations="$2"
  local results_file="$3"
  printf '{"component_id":"%s","iterations":%s,"scenarios":[]}\n' "$component" "$iterations" > "$results_file"
}
`);
	const preflightHelper = path.join(root, 'preflight-helper.sh');
	fs.writeFileSync(preflightHelper, '#!/usr/bin/env bash\nhomeboy_require_bash_version() { :; }\n');
	const resolveContextHelper = path.join(root, 'resolve-context-helper.sh');
	fs.writeFileSync(resolveContextHelper, `#!/usr/bin/env bash
homeboy_resolve_context() {
  PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
  COMPONENT_ID="$HOMEBOY_COMPONENT_ID"
}
`);

	const runnerResult = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
		cwd: componentPath,
		encoding: 'utf8',
		env: {
			...process.env,
			HOMEBOY_BENCH_EXTRA_WORKLOADS: benchWorkload,
			HOMEBOY_BENCH_ITERATIONS: '1',
			HOMEBOY_BENCH_RESULTS_FILE: path.join(root, 'results.json'),
			HOMEBOY_BENCH_WARMUP_ITERATIONS: '0',
			HOMEBOY_COMPONENT_ID: 'primitive-fixture',
			HOMEBOY_COMPONENT_PATH: componentPath,
			HOMEBOY_EXTENSION_PATH: extensionPath,
			HOMEBOY_RUNTIME_BASH_PREFLIGHT: preflightHelper,
			HOMEBOY_RUNTIME_BENCH_HELPER_SH: benchHelper,
			HOMEBOY_RUNTIME_RESOLVE_CONTEXT: resolveContextHelper,
			HOMEBOY_WP_CODEBOX_BIN: staleBin,
		},
	});

	assert.equal(runnerResult.status, 1);
	assert.match(runnerResult.stderr, /external-http-guardrail bench primitive is not available/);
	assert.doesNotMatch(runnerResult.stderr, /process\.exit\(99\)/);

	const overrideResult = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
		cwd: componentPath,
		encoding: 'utf8',
		env: {
			...process.env,
			HOMEBOY_BENCH_EXTRA_WORKLOADS: benchWorkload,
			HOMEBOY_BENCH_ITERATIONS: '1',
			HOMEBOY_BENCH_RESULTS_FILE: path.join(root, 'override-results.json'),
			HOMEBOY_BENCH_WARMUP_ITERATIONS: '0',
			HOMEBOY_COMPONENT_ID: 'primitive-fixture',
			HOMEBOY_COMPONENT_PATH: componentPath,
			HOMEBOY_EXTENSION_PATH: extensionPath,
			HOMEBOY_RUNTIME_BASH_PREFLIGHT: preflightHelper,
			HOMEBOY_RUNTIME_BENCH_HELPER_SH: benchHelper,
			HOMEBOY_RUNTIME_RESOLVE_CONTEXT: resolveContextHelper,
			HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: currentBin }),
			HOMEBOY_WP_CODEBOX_BIN: staleBin,
		},
	});

	assert.equal(overrideResult.status, 99);
	assert.doesNotMatch(overrideResult.stderr, /external-http-guardrail bench primitive is not available/);

	console.log('WP Codebox bench primitive preflight smoke passed');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
