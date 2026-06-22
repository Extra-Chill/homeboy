'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const Module = require('node:module');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(
	__dirname,
	'..',
	'..',
	'tests',
	'fixtures',
	'wp-codebox-core-runtime-contract.cjs'
);

const { runtimeContractSchemas } = require('../../agent-runtimes/wp-codebox');

const originalLoad = Module._load;
Module._load = function loadWithoutRuntimeAgentCi(request, parent, isMain) {
	if (request.includes('runtime-agent-ci')) {
		throw new Error(`WordPress fuzz runner must be installable without runtime-agent-ci: ${request}`);
	}
	return originalLoad.call(this, request, parent, isMain);
};

const {
	HOMEBOY_FUZZ_CAMPAIGN_SCHEMA,
	WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
	buildWordPressFuzzRunnerResult,
	runWordPressFuzzRunnerResult,
} = require('../lib/wordpress-fuzz-runner');

Module._load = originalLoad;

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
assert.equal(result.wp_codebox_input.goal, 'Run WordPress fuzz suite workload-from-env and return the declared fuzz artifacts.');
assert.equal(result.wp_codebox_input.cases[0].target_id, 'rest-posts');
assert.equal(result.wp_codebox_task_request.executor.config.runtime_task.ability, 'wp-codebox/run-fuzz-suite');
assert.equal(result.wp_codebox_plan_recipe.fuzzRun, undefined);
assert.equal(result.wp_codebox_plan_recipe.fuzzSuite.cases[0].case_id, 'get-posts');
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
		assert.equal(request.executor.config.runtime_task.ability, 'wp-codebox/run-fuzz-suite');
		assert.equal(request.executor.config.runtime_task.input.schema, 'wp-codebox/fuzz-suite/v1');
		assert.equal(request.executor.config.runtime_task.input.goal, 'Run WordPress fuzz suite generic-wordpress-workload and return the declared fuzz artifacts.');
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
const { discoverWpCodeboxBin, wpCodeboxCommand, wpCodeboxRuntimeEnv } = require(runnerPath);
fs.writeFileSync(workloadPath, `${JSON.stringify(workload)}\n`);

assert.equal(fs.statSync(runnerPath).mode & 0o111, 0o111, 'fuzz runner script must be executable');
assert.equal(
	wpCodeboxRuntimeEnv({
		HOMEBOY_SETTINGS_WP_CODEBOX_CORE_MODULE: '/runner/wp-codebox/packages/runtime-core/dist/contracts.js',
	}).HOMEBOY_WP_CODEBOX_CORE_MODULE,
	'/runner/wp-codebox/packages/runtime-core/dist/contracts.js',
	'Lab-exported per-setting env should provide the WP Codebox core module path'
);

const codeboxInstallRoot = path.join(tempDir, 'wp-codebox-install');
const cachedCodeboxBin = path.join(codeboxInstallRoot, 'source', 'packages', 'cli', 'dist', 'index.js');
fs.mkdirSync(path.dirname(cachedCodeboxBin), { recursive: true });
fs.writeFileSync(cachedCodeboxBin, '#!/usr/bin/env node\n');
assert.equal(
	discoverWpCodeboxBin({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: codeboxInstallRoot }),
	cachedCodeboxBin,
	'Fuzz runner should prefer the cached WP Codebox CLI over stale binaries on PATH'
);
assert.equal(
	wpCodeboxCommand({
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: codeboxInstallRoot,
		HOMEBOY_SETTINGS_WP_CODEBOX_BIN: '/stale/wp-codebox',
	}),
	cachedCodeboxBin,
	'Homeboy-managed WP Codebox cache should beat stale persisted settings'
);
assert.equal(
	wpCodeboxCommand({
		HOMEBOY_WP_CODEBOX_BIN: '/explicit/wp-codebox',
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: codeboxInstallRoot,
	}),
	'/explicit/wp-codebox',
	'Explicit WP Codebox binary env should override cache discovery'
);

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

const fakeCodeboxBin = path.join(tempDir, 'packages/cli/dist/fake-wp-codebox.js');
fs.mkdirSync(path.dirname(fakeCodeboxBin), { recursive: true });
const dispatchResultsPath = path.join(tempDir, 'dispatch-results.json');
fs.writeFileSync(fakeCodeboxBin, `#!/usr/bin/env node
const fs = require('node:fs');
const inputFile = process.argv[process.argv.indexOf('--input-file') + 1];
const command = process.argv[2];
const request = JSON.parse(fs.readFileSync(inputFile, 'utf8'));
if (command !== 'run-fuzz-suite') {
  process.stderr.write('expected public run-fuzz-suite command');
  process.exit(1);
}
if (request.schema !== 'wp-codebox/fuzz-suite/v1' || request.metadata?.homeboy_agent_task_request?.task_id !== 'dispatch-cli-run') {
  process.stderr.write('invalid public fuzz-suite input');
  process.exit(1);
}
process.stdout.write(JSON.stringify({
  schema: 'wp-codebox/fuzz-suite-result/v1',
  request_id: request.id,
  status: 'succeeded',
  artifactRefs: [{ path: 'fake/fuzz-report.json', kind: 'report', contentType: 'application/json' }],
  coverage_summary: { surface_count: 1, exercised_count: 1 }
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

const legacyCodeboxBin = path.join(tempDir, 'packages/cli/dist/fake-legacy-wp-codebox.js');
fs.writeFileSync(legacyCodeboxBin, `#!/usr/bin/env node
const fs = require('node:fs');
const command = process.argv[2];
if (command === 'run-fuzz-suite') {
  process.stderr.write('unknown command: run-fuzz-suite');
  process.exit(1);
}
const inputFile = process.argv[process.argv.indexOf('--input-file') + 1];
const request = JSON.parse(fs.readFileSync(inputFile, 'utf8'));
if (command !== 'run-agent-task' || request.schema !== '${runtimeContractSchemas().agentTask.runRequest}' || request.task_id !== 'legacy-dispatch-cli-run') {
  process.stderr.write('invalid run-agent-task fallback input');
  process.exit(1);
}
if (request.runtime_task || request.artifact_declarations || request.sandbox_tool_policy || request.extra_plugins) {
  process.stderr.write('fallback rebuilt the Codebox task payload instead of delegating to the adapter');
  process.exit(1);
}
if (request.task_input?.schema !== 'wp-codebox/task-input/v1') {
  process.stderr.write('missing delegated task input');
  process.exit(1);
}
if (request.task_input?.runtime_task?.ability !== 'wp-codebox/run-fuzz-suite' || request.task_input?.runtime_task?.input?.schema !== 'wp-codebox/fuzz-suite/v1') {
  process.stderr.write('missing fuzz runtime task');
  process.exit(1);
}
if (request.task_input?.artifact_declarations?.[0]?.name !== 'wp-codebox-fuzz-suite-result' || !request.task_input?.expected_artifacts?.includes('wordpress-fuzz-coverage')) {
  process.stderr.write('missing delegated artifact contract');
  process.exit(1);
}
if (request.task_input?.parent_request?.task_id !== 'legacy-dispatch-cli-run') {
  process.stderr.write('missing parent request metadata');
  process.exit(1);
}
process.stdout.write(JSON.stringify({
  success: true,
  agent_task_run_result: {
    schema: '${runtimeContractSchemas().agentTask.runResult}',
    result: {
      schema: 'wp-codebox/fuzz-suite-result/v1',
      request_id: request.task_id,
      status: 'succeeded',
      artifactRefs: [{ path: 'legacy/fuzz-report.json', kind: 'report', contentType: 'application/json' }],
      coverage_summary: { surface_count: 1, exercised_count: 1 }
    }
  }
}));
`);
fs.chmodSync(legacyCodeboxBin, 0o755);

const legacyDispatchCli = spawnSync(runnerPath, [], {
	encoding: 'utf8',
	env: {
		...process.env,
		HOMEBOY_WP_CODEBOX_BIN: legacyCodeboxBin,
		HOMEBOY_WP_CODEBOX_PLUGIN_PATH: path.join(tempDir, 'packages/wordpress-plugin'),
		HOMEBOY_FUZZ_WORKLOAD_PATH: workloadPath,
		HOMEBOY_FUZZ_WORKLOAD_ID: 'legacy-dispatch-cli-workload',
		HOMEBOY_FUZZ_RUN_ID: 'legacy-dispatch-cli-run',
	},
});

assert.equal(legacyDispatchCli.status, 0, legacyDispatchCli.stderr);
const legacyDispatchCliResult = JSON.parse(legacyDispatchCli.stdout);
assert.equal(legacyDispatchCliResult.succeeded, true);
assert.equal(legacyDispatchCliResult.homeboy_fuzz_campaign.metadata.artifact_refs[0].path, 'legacy/fuzz-report.json');

dispatchPromise.then(() => {
	console.log('WordPress fuzz runner smoke passed.');
}).catch((error) => {
	console.error(error);
	process.exit(1);
});
