'use strict';

const assert = require('node:assert/strict');

const {
	DEFAULT_FUZZ_RUN_ABILITY,
	DEFAULT_FUZZ_RUN_ARTIFACT_DECLARATIONS,
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
assert.deepEqual(taskRequest.artifact_declarations, DEFAULT_FUZZ_RUN_ARTIFACT_DECLARATIONS);
assert.deepEqual(
	taskRequest.artifact_declarations.filter((artifact) => artifact.required === true).map((artifact) => artifact.name),
	taskRequest.expected_artifacts
);
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
				wordpress_fuzz_result: {
					schema: 'wordpress-fuzz-result/v1',
					id: 'normalized-result',
					plan_id: 'generic-plan',
					status: 'passed',
					cases: [
						{
							id: 'case-000',
							target_id: 'target-rest',
							surface_id: 'surface-rest',
							operation_id: 'rest:list-posts',
							status: 'passed',
							role_boundary: { role: 'subscriber', outcome: 'allowed_as_expected' },
							db_query: { query_count: 1, rows_examined: 2, duration_ms: 3 },
							http_guardrail: { blocked: 1 },
						},
						{
							id: 'case-001',
							target_id: 'target-admin',
							surface_id: 'surface-admin',
							operation_id: 'admin:settings',
							status: 'skipped',
							skip_reason: 'capability_unavailable',
							destructive_reason: 'mutating_action',
							admin_browser: { errors: [{ message: 'blocked navigation' }] },
						},
					],
					provenance: { workload_manifest: 'workloads/generic-wordpress-fuzz.json' },
				},
				artifacts: {
					fuzz_report: { path: 'reports/fuzz-report.json', content_type: 'application/json' },
					coverage: { path: 'reports/coverage.json', content_type: 'application/json', size_bytes: 123 },
					normalized_fuzz_result: { path: 'reports/wordpress-fuzz-result.json', content_type: 'application/json' },
					fuzz_case: { path: 'cases/case-000.json', case_id: 'case-000' },
					placeholder_case: { name: 'placeholder-only' },
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
	assert.equal(summary.wordpress_fuzz_result.summary.surface_count, 2);
	assert.equal(summary.wordpress_fuzz_result.summary.operation_count, 2);
	assert.equal(summary.wordpress_fuzz_result.summary.skipped_reason_codes.capability_unavailable, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.destructive_reason_codes.mutating_action, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.role_boundary_outcomes.by_outcome.allowed_as_expected, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.db_query_metrics.query_count, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.admin_browser_errors.errors, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.http_guardrail_outcomes.blocked, 1);
	assert.equal(summary.wordpress_fuzz_result.provenance.workload_manifest, 'workloads/generic-wordpress-fuzz.json');
	assert.equal(summary.wordpress_fuzz_result.artifacts.some((artifact) => artifact.role === 'coverage'), true);
	assert.equal(summary.wordpress_fuzz_result.artifacts.some((artifact) => artifact.name === 'placeholder-only'), false);
	assert.equal(summary.coverage_gaps[0].status, 'skipped');
	assert.deepEqual(summary.artifacts.map((artifact) => artifact.role), ['fuzz_report', 'coverage', 'normalized_fuzz_result', 'fuzz_case', 'failing_case', 'case_artifact', 'repro_case']);
	assert.equal(summary.artifacts[0].semantic_key, 'fuzz.report');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'coverage').semantic_key, 'fuzz.coverage');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'coverage').size_bytes, 123);
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'normalized_fuzz_result').semantic_key, 'fuzz.result.normalized');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'fuzz_case').semantic_key, 'fuzz.case');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'fuzz_case').case_id, 'case-000');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'failing_case').semantic_key, 'fuzz.case.failing');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'case_artifact').semantic_key, 'fuzz.case.artifact');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'repro_case').semantic_key, 'fuzz.case.repro');
	assert.equal(summary.artifacts.some((artifact) => artifact.name === 'placeholder-only'), false);

	const normalized = normalizeWpCodeboxFuzzRunResult({ status: 'failed', failures: [{ message: 'boom' }] });
	assert.equal(normalized.succeeded, false);
	assert.equal(normalized.failures[0].message, 'boom');

	console.log('wp-codebox fuzz-run smoke passed');
}).catch((error) => {
	console.error(error);
	process.exit(1);
});
