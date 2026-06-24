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
} = require('../lib/wordpress-fuzz-runner');

const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-fuzz-codebox-contract-'));
const workloadPath = path.join(tempDir, 'workload.json');
const resultsPath = path.join(tempDir, 'campaign.json');
const observedRequestPath = path.join(tempDir, 'observed-fuzz-suite-request.json');
const emptyCodeboxInstallRoot = path.join(tempDir, 'empty-wp-codebox-install');
const fakeCodeboxBin = path.join(tempDir, 'wp-codebox');
const runnerPath = path.join(__dirname, '..', 'scripts', 'fuzz', 'fuzz-runner.cjs');

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
	metadata: { canary: 'wp-codebox-fuzz-suite-contract' },
};

fs.writeFileSync(workloadPath, `${JSON.stringify(workload, null, 2)}\n`);
fs.mkdirSync(emptyCodeboxInstallRoot, { recursive: true });
fs.writeFileSync(fakeCodeboxBin, `#!/usr/bin/env node
const fs = require('node:fs');

const command = process.argv[2];
const inputFileIndex = process.argv.indexOf('--input-file');
if (command !== 'run-fuzz-suite' || inputFileIndex < 0 || !process.argv.includes('--json')) {
  process.stderr.write('expected public run-fuzz-suite --input-file <file> --json invocation');
  process.exit(1);
}

const request = JSON.parse(fs.readFileSync(process.argv[inputFileIndex + 1], 'utf8'));
fs.writeFileSync(${JSON.stringify(observedRequestPath)}, JSON.stringify(request, null, 2));

if (request.schema !== 'wp-codebox/fuzz-suite/v1') {
  process.stderr.write('expected wp-codebox/fuzz-suite/v1 request');
  process.exit(1);
}
if (request.id !== 'contract-run' || request.cases?.[0]?.target_id !== 'rest-posts-target') {
  process.stderr.write('fuzz suite request was not built from the Homeboy workload');
  process.exit(1);
}
if (request.metadata?.homeboy_agent_task_request?.executor?.config?.runtime_task?.ability !== 'wp-codebox/run-fuzz-suite') {
  process.stderr.write('missing Homeboy agent task request metadata for the public fuzz-suite call');
  process.exit(1);
}
if (request.metadata?.homeboy_agent_task_request?.executor?.config?.runtime_task?.input?.schema !== 'wp-codebox/fuzz-suite/v1') {
  process.stderr.write('Homeboy task request did not carry a fuzz-suite input');
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
		HOMEBOY_WP_CODEBOX_FUZZ_DISPATCH: 'legacy-codebox-bin',
		HOMEBOY_WP_CODEBOX_BIN: fakeCodeboxBin,
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: emptyCodeboxInstallRoot,
		HOMEBOY_FUZZ_WORKLOAD_PATH: workloadPath,
		HOMEBOY_FUZZ_WORKLOAD_ID: 'contract-workload',
		HOMEBOY_FUZZ_RUN_ID: 'contract-run',
		HOMEBOY_FUZZ_SEED: 'seed-contract',
		HOMEBOY_FUZZ_MAX_DURATION: '5',
		HOMEBOY_FUZZ_RESULTS_FILE: resultsPath,
	},
});

assert.equal(cli.status, 0, cli.stderr || cli.stdout);

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
	['fuzz.report', 'fuzz.coverage']
);

const observedRequest = JSON.parse(fs.readFileSync(observedRequestPath, 'utf8'));
assert.equal(observedRequest.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(observedRequest.metadata.homeboy_agent_task_request.task_id, 'contract-run');
assert.equal(observedRequest.metadata.homeboy_agent_task_request.expected_artifacts[0], 'wp-codebox-fuzz-suite-result');
assert.equal(observedRequest.metadata.homeboy_agent_task_request.artifact_declarations[0].semantic_key, 'fuzz.result.normalized');
assert.equal(observedRequest.metadata.homeboy_agent_task_request.expected_artifacts.includes('case-log'), true);
assert.equal(observedRequest.metadata.homeboy_agent_task_request.expected_artifacts.includes('replay-data'), true);
assert.equal(observedRequest.metadata.homeboy_agent_task_request.expected_artifacts.includes('coverage-summary'), true);

const campaign = JSON.parse(fs.readFileSync(resultsPath, 'utf8'));
assert.equal(campaign.schema, HOMEBOY_FUZZ_CAMPAIGN_SCHEMA);
assert.equal(campaign.metadata.wordpress_fuzz_result.id, 'contract-result');
