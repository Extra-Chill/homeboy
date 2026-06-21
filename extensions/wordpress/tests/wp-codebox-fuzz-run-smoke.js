'use strict';

const assert = require('node:assert/strict');

const {
	DEFAULT_FUZZ_RUN_ABILITY,
	WP_CODEBOX_FUZZ_RUN_SCHEMA,
	normalizeWpCodeboxFuzzRunResult,
	runWpCodeboxFuzzRun,
	wpCodeboxFuzzRunInput,
	wpCodeboxFuzzRunTaskRequest,
} = require('../lib/wp-codebox-fuzz-run');

const input = wpCodeboxFuzzRunInput({
	id: 'fuzz-smoke',
	target: { type: 'wordpress-plugin', slug: 'sample-plugin' },
	workload: { entry: 'rest-routes' },
	cases: [{ method: 'GET', path: '/wp/v2/posts' }],
	seeds: [{ name: 'sample-post' }],
	limits: { max_cases: 1 },
	coverage: { hooks: true, db: true },
	runtimeProfile: { components: [{ name: 'sample-plugin', path: '/workspace/sample-plugin' }] },
	metadata: { scenario: 'smoke' },
});

assert.equal(input.schema, WP_CODEBOX_FUZZ_RUN_SCHEMA);
assert.equal(input.target.slug, 'sample-plugin');
assert.deepEqual(input.limits, { max_cases: 1 });

const taskRequest = wpCodeboxFuzzRunTaskRequest({
	taskId: 'wp-codebox-fuzz-run-smoke',
	input,
	provider: 'codex',
	runtimeId: 'wp-codebox',
});

assert.equal(taskRequest.executor.backend, 'codebox');
assert.equal(taskRequest.executor.runtime, 'wp-codebox');
assert.equal(taskRequest.executor.config.runtime_task.ability, DEFAULT_FUZZ_RUN_ABILITY);
assert.equal(taskRequest.executor.config.runtime_task.input.schema, WP_CODEBOX_FUZZ_RUN_SCHEMA);
assert.deepEqual(taskRequest.expected_artifacts, ['wp-codebox-fuzz-run-result', 'wordpress-fuzz-coverage']);
assert(!JSON.stringify(taskRequest).includes('woocommerce'), 'fuzz-run helper must stay product-agnostic');

let invoked = false;
runWpCodeboxFuzzRun({
	taskId: 'wp-codebox-fuzz-run-delegation-smoke',
	input,
	runFuzzRun: async (request) => {
		invoked = true;
		assert.equal(request.executor.config.runtime_task.input.schema, WP_CODEBOX_FUZZ_RUN_SCHEMA);
		return {
			json: {
				schema: 'wp-codebox/fuzz-run-result/v1',
				request_id: request.task_id,
				status: 'succeeded',
				coverage_summary: {
					surface_count: 3,
					exercised_count: 1,
					skipped_count: 1,
					failed_count: 1,
				},
				coverage_gaps: [{ id: 'route:/wp/v2/users', type: 'rest_route', status: 'skipped' }],
				coverage: { hooks: { actions: { init: 1 } } },
				artifacts: {
					fuzz_report: { path: 'reports/fuzz-report.json', content_type: 'application/json' },
					fuzz_case: { path: 'cases/case-000.json', case_id: 'case-000' },
					failing_case: { path: 'cases/failing-case.json', case_id: 'case-002' },
					case_artifact: { path: 'cases/case-001.json', case_id: 'case-001' },
					repro_case: { path: 'repro/case-002.js', case_id: 'case-002' },
				},
			},
		};
	},
}).then((summary) => {
	assert.equal(invoked, true);
	assert.equal(summary.schema, 'homeboy/wordpress-codebox-fuzz-run-consumer/v1');
	assert.equal(summary.delegated_schema, WP_CODEBOX_FUZZ_RUN_SCHEMA);
	assert.equal(summary.succeeded, true);
	assert.equal(summary.coverage.hooks.actions.init, 1);
	assert.equal(summary.coverage_summary.surface_count, 3);
	assert.equal(summary.coverage_summary.exercised_count, 1);
	assert.equal(summary.coverage_gaps[0].status, 'skipped');
	assert.deepEqual(summary.artifacts.map((artifact) => artifact.role), ['fuzz_report', 'fuzz_case', 'failing_case', 'case_artifact', 'repro_case']);
	assert.equal(summary.artifacts[0].semantic_key, 'fuzz.report');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'fuzz_case').semantic_key, 'fuzz.case');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'failing_case').semantic_key, 'fuzz.case.failing');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'case_artifact').semantic_key, 'fuzz.case.artifact');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'repro_case').semantic_key, 'fuzz.case.repro');

	const normalized = normalizeWpCodeboxFuzzRunResult({ status: 'failed', failures: [{ message: 'boom' }] });
	assert.equal(normalized.succeeded, false);
	assert.equal(normalized.failures[0].message, 'boom');

	console.log('wp-codebox fuzz-run smoke passed');
}).catch((error) => {
	console.error(error);
	process.exit(1);
});
