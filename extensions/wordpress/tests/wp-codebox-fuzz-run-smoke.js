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
				coverage: { hooks: { actions: { init: 1 } } },
				artifacts: { directory: '/tmp/codebox-fuzz-artifacts' },
			},
		};
	},
}).then((summary) => {
	assert.equal(invoked, true);
	assert.equal(summary.schema, 'homeboy/wordpress-codebox-fuzz-run-consumer/v1');
	assert.equal(summary.delegated_schema, WP_CODEBOX_FUZZ_RUN_SCHEMA);
	assert.equal(summary.succeeded, true);
	assert.equal(summary.coverage.hooks.actions.init, 1);

	const normalized = normalizeWpCodeboxFuzzRunResult({ status: 'failed', failures: [{ message: 'boom' }] });
	assert.equal(normalized.succeeded, false);
	assert.equal(normalized.failures[0].message, 'boom');

	console.log('wp-codebox fuzz-run smoke passed');
}).catch((error) => {
	console.error(error);
	process.exit(1);
});
