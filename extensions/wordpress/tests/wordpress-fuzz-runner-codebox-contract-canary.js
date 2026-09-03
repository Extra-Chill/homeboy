'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
	HOMEBOY_FUZZ_CAMPAIGN_SCHEMA,
	WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
	readWordPressFuzzRunnerEnv,
} = require('../lib/wordpress-fuzz-runner');

const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-fuzz-codebox-contract-'));
const workloadPath = path.join(tempDir, 'workload.json');
const resultsPath = path.join(tempDir, 'campaign.json');
const executionRequestPath = path.join(tempDir, 'execution-request.json');
const observedRequestPath = path.join(tempDir, 'observed-fuzz-suite-request.json');
const observedArgvPath = path.join(tempDir, 'observed-fuzz-suite-argv.json');
const artifactRoot = path.join(tempDir, 'artifacts');
const emptyCodeboxInstallRoot = path.join(tempDir, 'empty-wp-codebox-install');
const checkoutRoot = path.join(tempDir, 'checkout');
const pluginRoot = path.join(checkoutRoot, 'plugins', 'woocommerce');
const fakeCodeboxCoreModule = path.join(tempDir, 'wp-codebox-core-contracts.cjs');
const fixtureCodeboxCoreModule = path.join(__dirname, '..', '..', 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const fakeCodeboxBin = path.join(tempDir, 'wp-codebox');
const runnerPath = path.join(__dirname, '..', 'scripts', 'fuzz', 'fuzz-runner.cjs');
const { wpCodeboxRuntimeCommand } = require(runnerPath);

const workload = {
	id: 'contract-workload',
	plan: {
		schema: 'wordpress-fuzz-plan/v1',
		id: 'contract-plan',
		targets: [
			{
				id: 'rest-posts-target',
				surface_id: 'route:/wp/v2/posts',
				cases: [
					{
						id: 'list-posts',
						operation_id: 'rest:list-posts',
						method: 'GET',
						path: '/wp/v2/posts',
					},
				],
			},
		],
	},
	target: { type: 'wordpress-plugin', slug: 'woocommerce', component: 'woocommerce' },
	cases: [{ id: 'contract-workload:default', intent: { plugin: { activation: 'woocommerce/woocommerce.php' } } }],
	metadata: {
		canary: 'wp-codebox-fuzz-suite-contract',
		fixture: { component: 'woocommerce', activation: 'woocommerce/woocommerce.php' },
		homeboy_runtime_context: {
			schema: 'homeboy/fuzz-workload-runtime-context/v1',
			rig_id: 'woocommerce-performance',
			components: {
				woocommerce: {
					path: checkoutRoot,
					extensions: {
						wordpress: {
							wp_codebox_source_root: '${env.HOMEBOY_RIG_COMPONENT_CHECKOUT_ROOT__WOOCOMMERCE_PERFORMANCE__WOOCOMMERCE}',
							wp_codebox_source_subpath: 'plugins/woocommerce',
							wp_codebox_mount_slug: 'woocommerce',
							wp_codebox_plugin_file: 'woocommerce/woocommerce.php',
						},
					},
				},
			},
		},
	},
};

const executionRequest = {
	selection: { include: ['rest:*'], exclude: ['admin:*'] },
	budgets: { max_cases: 9, max_duration_seconds: 5 },
	action_model: { name: 'rest-request', mutation: 'isolated' },
	exploration_policy: { strategy: 'coverage-guided', max_depth: 3 },
};

fs.writeFileSync(workloadPath, `${JSON.stringify(workload, null, 2)}\n`);
fs.writeFileSync(executionRequestPath, `${JSON.stringify(executionRequest, null, 2)}\n`);
fs.mkdirSync(path.join(emptyCodeboxInstallRoot, 'source', 'packages', 'cli'), { recursive: true });
fs.mkdirSync(pluginRoot, { recursive: true });
fs.writeFileSync(path.join(pluginRoot, 'woocommerce.php'), '<?php\n/**\n * Plugin Name: WooCommerce\n */\n');
fs.writeFileSync(fakeCodeboxCoreModule, `module.exports = require(${JSON.stringify(fixtureCodeboxCoreModule)});\n`);
fs.writeFileSync(fakeCodeboxBin, `#!/usr/bin/env node
const fs = require('node:fs');
const { runtimeContractManifest } = require(${JSON.stringify(fakeCodeboxCoreModule)});

const commandOffset = process.argv[2] === 'codebox' ? 1 : 0;
const command = process.argv[2 + commandOffset];
const inputFileIndex = process.argv.indexOf('--input-file');
if (process.argv.includes('--version')) {
  process.stdout.write('0.21.0');
  process.exit(0);
}
if (command === 'runtime' && process.argv[3 + commandOffset] === 'descriptor' && process.argv.includes('--json')) {
  process.stdout.write(JSON.stringify({
     schema: 'wp-codebox/runtime-descriptor/v1',
     readiness: { status: 'available', browserRuntime: { status: 'ready' } },
     contractManifest: runtimeContractManifest(),
  }));
  process.exit(0);
}
if (command === 'fuzz' && process.argv[3] === 'readiness' && process.argv.includes('--format=json')) {
  process.stderr.write('production dispatch must not probe fuzz readiness');
  process.exit(2);
}
if (command === 'run-fuzz-suite' && process.argv.includes('--help')) {
	process.stderr.write('production dispatch must not probe run-fuzz-suite help');
	process.exit(2);
}
if (command === 'run-wordpress-workload' && process.argv.includes('--help')) {
	process.stderr.write('production dispatch must not probe run-wordpress-workload help');
	process.exit(2);
}
if (command !== 'run-fuzz-suite' || inputFileIndex < 0 || (!process.argv.includes('--format=json') && !process.argv.includes('--json'))) {
	process.stderr.write('expected public run-fuzz-suite --input-file <file> JSON invocation');
	process.exit(1);
}

const request = JSON.parse(fs.readFileSync(process.argv[inputFileIndex + 1], 'utf8'));
fs.writeFileSync(${JSON.stringify(observedRequestPath)}, JSON.stringify(request, null, 2));
fs.writeFileSync(${JSON.stringify(observedArgvPath)}, JSON.stringify(process.argv.slice(1), null, 2));

if (request.schema !== 'wp-codebox/fuzz-suite/v1') {
  process.stderr.write('expected wp-codebox/fuzz-suite/v1 request');
  process.exit(1);
}
if (request.id !== 'contract-run' || request.cases?.[0]?.target_id !== 'rest-posts-target') {
  process.stderr.write('fuzz suite request was not built from the Homeboy workload');
  process.exit(1);
}
if (request.metadata?.homeboy_wp_codebox_fuzz_execution?.schema !== 'homeboy/wp-codebox-fuzz-execution/v1') {
  process.stderr.write('missing Homeboy direct fuzz execution metadata for the public fuzz-suite call');
  process.exit(1);
}
if (request.metadata?.homeboy_wp_codebox_fuzz_execution?.input?.schema !== 'wp-codebox/fuzz-suite/v1') {
  process.stderr.write('Homeboy direct execution request did not carry a fuzz-suite input');
  process.exit(1);
}
if (request.metadata?.homeboy_agent_task_request) {
  process.stderr.write('fuzz execution must not route through Homeboy agent-task metadata');
  process.exit(1);
}
if (JSON.stringify(request.execution_request) !== JSON.stringify(${JSON.stringify(executionRequest)})) {
  process.stderr.write('fuzz suite request did not preserve the portable execution request');
  process.exit(1);
}

process.stdout.write(JSON.stringify({
  schema: 'wp-codebox/fuzz-suite-result/v1',
  request_id: request.id,
  status: 'succeeded',
  summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
  coverage_summary: { surface_count: 1, exercised_count: 1, skipped_count: 0, failed_count: 0 },
  artifactRefs: [
    { path: 'artifacts/fuzz-report.json', kind: 'report', contentType: 'application/json' },
    { path: 'artifacts/coverage.json', kind: 'coverage', contentType: 'application/json' }
  ],
  wordpress_fuzz_result: {
    schema: 'wordpress-fuzz-result/v1',
    id: 'contract-result',
    plan_id: 'contract-plan',
    status: 'passed',
    cases: [{ id: 'list-posts', target_id: 'rest-posts-target', surface_id: 'route:/wp/v2/posts', status: 'passed' }]
  }
}));
`);
fs.chmodSync(fakeCodeboxBin, 0o755);

const cli = spawnSync(runnerPath, [], {
	encoding: 'utf8',
	env: {
		...process.env,
		HOMEBOY_WP_CODEBOX_BIN: fakeCodeboxBin,
		HOMEBOY_WP_CODEBOX_CORE_MODULE: fakeCodeboxCoreModule,
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: emptyCodeboxInstallRoot,
		HOMEBOY_FUZZ_WORKLOAD_PATH: workloadPath,
		HOMEBOY_FUZZ_WORKLOAD_ID: 'contract-workload',
		HOMEBOY_FUZZ_RUN_ID: 'contract-run',
		HOMEBOY_FUZZ_SEED: 'seed-contract',
		HOMEBOY_FUZZ_MAX_DURATION: '5',
		HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE: executionRequestPath,
		HOMEBOY_FUZZ_RESULTS_FILE: resultsPath,
		HOMEBOY_FUZZ_ARTIFACTS_DIR: artifactRoot,
		HOMEBOY_RIG_COMPONENT_CHECKOUT_ROOT__WOOCOMMERCE_PERFORMANCE__WOOCOMMERCE: checkoutRoot,
	},
});

assert.equal(cli.status, 0, cli.stderr || cli.stdout);

assert.throws(
	() => wpCodeboxRuntimeCommand({
		HOME: tempDir,
		PATH: '',
		HOMEBOY_WP_CODEBOX_BIN: '',
		WP_CODEBOX_BIN: '',
		HOMEBOY_SETTINGS_WP_CODEBOX_BIN: '',
		HOMEBOY_SETTINGS_JSON: '{}',
		HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT: '',
		WP_CODEBOX_RUNTIME_COMPONENT: '',
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: emptyCodeboxInstallRoot,
	}),
	/wp_codebox_managed_binary_missing/,
	'an incomplete managed cache must be rejected before any fuzz command is spawned',
);

const result = JSON.parse(cli.stdout);
assert.equal(result.schema, WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA);
assert.equal(result.status, 'succeeded');
assert.equal(result.succeeded, true);
assert.equal(result.run_id, 'contract-run');
assert.equal(result.wp_codebox_input.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(result.wp_codebox_task_request.executor.config.runtime_task.ability, 'wp-codebox/run-fuzz-suite');
assert.equal(result.wp_codebox_task_request.executor.config.runtime_task.input.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(result.wp_codebox_result.result_schema, 'wp-codebox/fuzz-suite-result/v1');
assert.equal(result.wp_codebox_result.request_id, 'contract-run');
assert.equal(result.wp_codebox_result.wordpress_fuzz_result.schema, 'wordpress-fuzz-result/v1');
assert.equal(result.homeboy_fuzz_campaign.schema, HOMEBOY_FUZZ_CAMPAIGN_SCHEMA);
assert.equal(result.homeboy_fuzz_campaign.id, 'contract-run');
assert.equal(result.homeboy_fuzz_campaign.metadata.status, 'succeeded');
assert.equal(result.homeboy_fuzz_campaign.metadata.wp_codebox_result_schema, 'wp-codebox/fuzz-suite-result/v1');
assert.deepEqual(
	result.homeboy_fuzz_campaign.metadata.artifact_refs.map((artifact) => artifact.semantic_key),
	['fuzz.report', 'fuzz.coverage', 'fuzz.result.envelope']
);

const observedRequest = JSON.parse(fs.readFileSync(observedRequestPath, 'utf8'));
const observedArgv = JSON.parse(fs.readFileSync(observedArgvPath, 'utf8'));
assert.equal(observedArgv[0], fakeCodeboxBin, 'the explicit external pin is executed, not replaced by a managed candidate');
assert.deepEqual(observedArgv.slice(1, 4), ['run-fuzz-suite', '--runner-mode=runtime-backed', '--input-file']);
assert.deepEqual(observedArgv.slice(-2), ['--artifacts', artifactRoot]);
assert.equal(observedRequest.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(observedRequest.metadata.homeboy_wp_codebox_fuzz_execution.schema, 'homeboy/wp-codebox-fuzz-execution/v1');
assert.equal(observedRequest.metadata.homeboy_wp_codebox_fuzz_execution.task_id, 'contract-run');
assert.equal(observedRequest.metadata.homeboy_wp_codebox_fuzz_execution.expected_artifacts[0], 'wp-codebox-fuzz-suite-result');
assert.equal(observedRequest.metadata.homeboy_wp_codebox_fuzz_execution.artifact_declarations[0].semantic_key, 'fuzz.result.normalized');
assert.equal(observedRequest.metadata.homeboy_wp_codebox_fuzz_execution.expected_artifacts.includes('case-log'), true);
assert.equal(observedRequest.metadata.homeboy_wp_codebox_fuzz_execution.expected_artifacts.includes('replay-data'), true);
assert.equal(observedRequest.metadata.homeboy_wp_codebox_fuzz_execution.expected_artifacts.includes('coverage-summary'), true);
assert.equal(observedRequest.metadata.homeboy_agent_task_request, undefined);
assert.deepEqual(observedRequest.execution_request, executionRequest);
const observedPlugin = observedRequest.metadata.runtime_requirements.extra_plugins[0];
assert.equal(observedPlugin.source, checkoutRoot);
assert.equal(observedPlugin.sourceSubpath, 'plugins/woocommerce');
assert.equal(observedPlugin.mountSlug, 'woocommerce');
assert.equal(observedPlugin.pluginFile, 'woocommerce/woocommerce.php');
assert.notEqual(observedPlugin.pluginFile, 'woocommerce.php');

const campaign = JSON.parse(fs.readFileSync(resultsPath, 'utf8'));
assert.equal(campaign.schema, HOMEBOY_FUZZ_CAMPAIGN_SCHEMA);
assert.equal(campaign.metadata.wordpress_fuzz_result.id, 'contract-result');

assert.equal(readWordPressFuzzRunnerEnv({}).executionRequest, undefined);
assert.throws(
	() => readWordPressFuzzRunnerEnv({ HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE: path.join(tempDir, 'missing-request.json') }),
	/Unable to read HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE/,
);
const malformedRequestPath = path.join(tempDir, 'malformed-request.json');
fs.writeFileSync(malformedRequestPath, '{not-json');
assert.throws(
	() => readWordPressFuzzRunnerEnv({ HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE: malformedRequestPath }),
	/Invalid JSON in HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE/,
);
