'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const {
	HOMEBOY_FUZZ_CAMPAIGN_SCHEMA,
	WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
	buildWordPressFuzzRunnerResult,
	runWordPressFuzzRunnerResult,
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
assert.equal(result.wp_codebox_input.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(result.wp_codebox_input.cases[0].target_id, 'rest-posts');
assert.equal(result.wp_codebox_task_request.executor.config.runtime_task.ability, 'wp-codebox/fuzz-suite');
assert.equal(result.wp_codebox_plan_recipe.fuzzRun.cases[0].case_id, 'get-posts');
assert.equal(result.coverage.schema, 'homeboy/wordpress-fuzz-coverage-aggregate/v1');
assert.equal(result.coverage.totals.exercised, 1);
assert.equal(result.homeboy_fuzz_campaign.schema, HOMEBOY_FUZZ_CAMPAIGN_SCHEMA);
assert.equal(result.homeboy_fuzz_campaign.id, 'run-from-env');
assert.equal(result.homeboy_fuzz_campaign.safety_class, 'read_only');
assert.equal(result.homeboy_fuzz_campaign.metadata.status, 'failed');
assert.equal(result.homeboy_fuzz_campaign.metadata.wp_codebox_result_schema, 'wp-codebox/fuzz-suite-result/v1');
assert.equal(result.homeboy_fuzz_campaign.metadata.diagnostics[0].code, 'wp_codebox_fuzz_suite_execution_unsupported');
assert(!JSON.stringify(result).includes('woocommerce'), 'WordPress fuzz runner must stay product-agnostic');

const executedResult = buildWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		runId: 'executed-run',
	},
	workload: {
		...workload,
		wp_codebox_suite_result: {
			schema: 'wp-codebox/fuzz-suite-result/v1',
			suite: { id: 'generic-wordpress-plan' },
			status: 'succeeded',
			artifactRefs: [{ path: 'artifacts/replay.json', kind: 'replay' }],
		},
	},
});

assert.equal(executedResult.status, 'succeeded');
assert.equal(executedResult.succeeded, true);
assert.equal(executedResult.homeboy_fuzz_campaign.metadata.artifact_refs[0].path, 'artifacts/replay.json');

let dispatchedRequest;
const dispatchPromise = runWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		runId: 'dispatch-run',
	},
	workload,
	runRuntimeTask: async (request) => {
		dispatchedRequest = request;
		assert.equal(request.task_id, 'dispatch-run');
		assert.equal(request.executor.backend, 'codebox');
		assert.equal(request.executor.config.runtime_task.ability, 'wp-codebox/fuzz-suite');
		assert.equal(request.executor.config.runtime_task.input.schema, 'wp-codebox/fuzz-suite/v1');
		assert.equal(request.executor.config.runtime_task.input.cases[0].target_id, 'rest-posts');
		return {
			json: {
				schema: 'wp-codebox/fuzz-suite-result/v1',
				request_id: request.task_id,
				status: 'succeeded',
				coverage_summary: { surface_count: 1, exercised_count: 1 },
				artifactRefs: [
					{ path: 'dispatch/fuzz-report.json', kind: 'report', contentType: 'application/json' },
					{ path: 'dispatch/coverage.json', kind: 'coverage', contentType: 'application/json' },
				],
			},
		};
	},
}).then((dispatchedResult) => {
	assert(dispatchedRequest, 'runner should dispatch a Codebox fuzz suite request');
	assert.equal(dispatchedResult.status, 'succeeded');
	assert.equal(dispatchedResult.succeeded, true);
	assert.equal(dispatchedResult.wp_codebox_result.request_id, 'dispatch-run');
	assert.equal(dispatchedResult.wp_codebox_result.coverage_summary.surface_count, 1);
	assert.deepEqual(dispatchedResult.wp_codebox_result.artifacts.map((artifact) => artifact.role), ['fuzz_report', 'coverage']);
	assert.equal(dispatchedResult.homeboy_fuzz_campaign.metadata.artifact_refs[0].semantic_key, 'fuzz.report');
	assert.equal(dispatchedResult.homeboy_fuzz_campaign.metadata.artifact_refs[1].semantic_key, 'fuzz.coverage');
});

const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-fuzz-runner-'));
const workloadPath = path.join(tempDir, 'workload.json');
const resultsPath = path.join(tempDir, 'fuzz-results.json');
const runnerPath = path.join(__dirname, '..', 'scripts', 'fuzz', 'fuzz-runner.cjs');
fs.writeFileSync(workloadPath, `${JSON.stringify(workload)}\n`);

assert.equal(fs.statSync(runnerPath).mode & 0o111, 0o111, 'fuzz runner script must be executable');

const cli = spawnSync(runnerPath, [], {
	encoding: 'utf8',
	env: {
		...process.env,
		HOMEBOY_WP_CODEBOX_FUZZ_DISPATCH: '0',
		HOMEBOY_FUZZ_WORKLOAD_PATH: workloadPath,
		HOMEBOY_FUZZ_WORKLOAD_ID: 'cli-workload',
		HOMEBOY_FUZZ_RUN_ID: 'cli-run',
		HOMEBOY_FUZZ_SEED: 'cli-seed',
		HOMEBOY_FUZZ_MAX_DURATION: '15',
		HOMEBOY_FUZZ_RESULTS_FILE: resultsPath,
	},
});

assert.equal(cli.status, 0, cli.stderr);
const cliResult = JSON.parse(cli.stdout);
assert.equal(cliResult.schema, WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA);
assert.equal(cliResult.run_id, 'cli-run');
assert.equal(cliResult.succeeded, false);
assert.equal(cliResult.wp_codebox_input.metadata.limits.max_duration_seconds, 15);
const homeboyCampaign = JSON.parse(fs.readFileSync(resultsPath, 'utf8'));
assert.equal(homeboyCampaign.schema, HOMEBOY_FUZZ_CAMPAIGN_SCHEMA);
assert.equal(homeboyCampaign.id, 'cli-run');
assert.equal(homeboyCampaign.metadata.diagnostics[0].code, 'wp_codebox_fuzz_suite_execution_unsupported');

const fakeCodeboxBin = path.join(tempDir, 'fake-wp-codebox.js');
const dispatchResultsPath = path.join(tempDir, 'dispatch-results.json');
fs.writeFileSync(fakeCodeboxBin, `#!/usr/bin/env node
const fs = require('node:fs');
const inputFile = process.argv[process.argv.indexOf('--input-file') + 1];
const request = JSON.parse(fs.readFileSync(inputFile, 'utf8'));
if (request.schema !== 'wp-codebox/run-agent-task/v1' || !request.goal || !request.runtime_task) {
  process.stderr.write('invalid run-agent-task input');
  process.exit(1);
}
process.stdout.write(JSON.stringify({
  success: true,
  result: {
    schema: 'wp-codebox/fuzz-suite-result/v1',
    request_id: request.id,
    status: 'succeeded',
    artifactRefs: [{ path: 'fake/fuzz-report.json', kind: 'report', contentType: 'application/json' }],
    coverage_summary: { surface_count: 1, exercised_count: 1 }
  }
}));
`);
fs.chmodSync(fakeCodeboxBin, 0o755);

const dispatchCli = spawnSync(runnerPath, [], {
	encoding: 'utf8',
	env: {
		...process.env,
		HOMEBOY_WP_CODEBOX_BIN: fakeCodeboxBin,
		HOMEBOY_FUZZ_WORKLOAD_PATH: workloadPath,
		HOMEBOY_FUZZ_WORKLOAD_ID: 'dispatch-cli-workload',
		HOMEBOY_FUZZ_RUN_ID: 'dispatch-cli-run',
		HOMEBOY_FUZZ_RESULTS_FILE: dispatchResultsPath,
	},
});

assert.equal(dispatchCli.status, 0, dispatchCli.stderr);
const dispatchCliResult = JSON.parse(dispatchCli.stdout);
assert.equal(dispatchCliResult.succeeded, true);
assert.equal(dispatchCliResult.wp_codebox_result.request_id, 'dispatch-cli-run');
assert.equal(dispatchCliResult.homeboy_fuzz_campaign.metadata.artifact_refs[0].path, 'fake/fuzz-report.json');

dispatchPromise.then(() => {
	console.log('WordPress fuzz runner smoke passed.');
}).catch((error) => {
	console.error(error);
	process.exit(1);
});
