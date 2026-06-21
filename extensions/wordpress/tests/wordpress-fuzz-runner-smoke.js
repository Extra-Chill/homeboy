'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const {
	WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
	buildWordPressFuzzRunnerResult,
} = require('../lib/wordpress-fuzz-runner');

const workload = {
	id: 'generic-wordpress-workload',
	plan: {
		schema: 'wordpress-fuzz-plan/v1',
		id: 'generic-wordpress-plan',
		targets: [
			{
				id: 'rest-posts',
				surface_id: 'route:/wp/v2/posts',
				cases: [{ id: 'get-posts', method: 'GET', path: '/wp/v2/posts' }],
			},
		],
	},
	wp_codebox_plan: {
		id: 'codebox-plan',
		cases: [
			{
				case_id: 'get-posts',
				action: [{ command: 'wp-rest-request', args: ['GET', '/wp/v2/posts'] }],
			},
		],
	},
	coverage_artifacts: [
		{
			schema: 'homeboy/wordpress-fuzz-coverage/v1',
			hooks: { actions: { init: 1 } },
		},
	],
};

const result = buildWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		workloadId: 'workload-from-env',
		runId: 'run-from-env',
		seed: 'seed-123',
		maxDuration: '30',
	},
	workload,
});

assert.equal(result.schema, WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA);
assert.equal(result.status, 'failed');
assert.equal(result.succeeded, false);
assert.equal(result.run_id, 'run-from-env');
assert.equal(result.workload_id, 'workload-from-env');
assert.equal(result.seed, 'seed-123');
assert.equal(result.max_duration_seconds, 30);
assert.equal(result.wp_codebox_input.schema, 'wp-codebox/fuzz-run/v1');
assert.equal(result.wp_codebox_input.cases[0].target_id, 'rest-posts');
assert.equal(result.wp_codebox_task_request.executor.config.runtime_task.ability, 'wp-codebox/fuzz-run');
assert.equal(result.wp_codebox_plan_recipe.fuzzRun.cases[0].case_id, 'get-posts');
assert.equal(result.coverage.schema, 'homeboy/wordpress-fuzz-coverage-aggregate/v1');
assert.equal(result.coverage.totals.exercised, 1);
assert(!JSON.stringify(result).includes('woocommerce'), 'WordPress fuzz runner must stay product-agnostic');

const executedResult = buildWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		runId: 'executed-run',
	},
	workload: {
		...workload,
		wp_codebox_result: {
			status: 'succeeded',
		},
	},
});

assert.equal(executedResult.status, 'succeeded');
assert.equal(executedResult.succeeded, true);

const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-fuzz-runner-'));
const workloadPath = path.join(tempDir, 'workload.json');
fs.writeFileSync(workloadPath, `${JSON.stringify(workload)}\n`);

const cli = spawnSync(process.execPath, [path.join(__dirname, '..', 'scripts', 'fuzz', 'fuzz-runner.cjs')], {
	encoding: 'utf8',
	env: {
		...process.env,
		HOMEBOY_FUZZ_WORKLOAD_PATH: workloadPath,
		HOMEBOY_FUZZ_WORKLOAD_ID: 'cli-workload',
		HOMEBOY_FUZZ_RUN_ID: 'cli-run',
		HOMEBOY_FUZZ_SEED: 'cli-seed',
		HOMEBOY_FUZZ_MAX_DURATION: '15',
	},
});

assert.equal(cli.status, 0, cli.stderr);
const cliResult = JSON.parse(cli.stdout);
assert.equal(cliResult.schema, WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA);
assert.equal(cliResult.run_id, 'cli-run');
assert.equal(cliResult.succeeded, false);
assert.equal(cliResult.wp_codebox_input.limits.max_duration_seconds, 15);

console.log('WordPress fuzz runner smoke passed.');
