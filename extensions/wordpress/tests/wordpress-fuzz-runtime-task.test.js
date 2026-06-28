'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_FUZZ_HOTSPOT_SUMMARY_SCHEMA,
	WORDPRESS_FUZZ_OBSERVATION_SET_SCHEMA,
	WORDPRESS_FUZZ_RUNTIME_TASK_REQUEST_SCHEMA,
	buildWordPressFuzzRuntimeTaskRequest,
	fuzzHotspotSummaryFromObservationSet,
	normalizeFuzzObservationSet,
	normalizeFuzzHotspotSummary,
	WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
} = require('../lib/wordpress-fuzz-runtime-task');
const {
	WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA,
	runWpCodeboxFuzzSuite,
	wpCodeboxFuzzRuntimeTaskRequest,
} = require('../lib/wp-codebox-fuzz-run');
const {
	aggregateWordPressFuzzCoverage,
	formatWordPressFuzzCoverageMarkdownReport,
} = require('../lib/wordpress-fuzz-coverage-aggregate');
const {
	buildWordPressFuzzMutationLifecycleContract,
} = require('../lib/wordpress-fuzz-mutation-lifecycle');

const genericRequest = buildWordPressFuzzRuntimeTaskRequest({
	taskId: 'generic-runtime-task',
	provider: { id: 'example-provider' },
	input: { schema: 'example/fuzz-input/v1' },
	providerRequest: { schema: 'example/provider-request/v1' },
});
assert.equal(genericRequest.schema, WORDPRESS_FUZZ_RUNTIME_TASK_REQUEST_SCHEMA);
assert.equal(genericRequest.provider.id, 'example-provider');
assert.equal(genericRequest.input.schema, 'example/fuzz-input/v1');
assert.equal(genericRequest.provider_request.schema, 'example/provider-request/v1');

const codeboxRuntimeRequest = wpCodeboxFuzzRuntimeTaskRequest({
	taskId: 'wp-codebox-runtime-task',
	input: { id: 'suite-input', cases: [{ id: 'case-1' }] },
});
assert.equal(codeboxRuntimeRequest.schema, WORDPRESS_FUZZ_RUNTIME_TASK_REQUEST_SCHEMA);
assert.equal(codeboxRuntimeRequest.provider.id, 'wp-codebox');
assert.equal(codeboxRuntimeRequest.provider_request.executor.backend, 'wp-codebox');
assert.equal(codeboxRuntimeRequest.input.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(codeboxRuntimeRequest.provider_request.schema, 'homeboy/agent-task-request/v1');
assert.equal(codeboxRuntimeRequest.provider_metadata.wp_codebox.ability, 'wp-codebox/run-fuzz-suite');

const normalizedHotspots = normalizeFuzzHotspotSummary({
	dimension: 'database',
	metric: 'query_count',
	unit: 'count',
	items: [{ surface: 'rest:/wp/v2/posts', operation: 'GET /wp/v2/posts', value: 14, rank: 1, sample_count: 2, evidence: [{ path: 'reports/query.json', semantic_key: 'fuzz.hotspot.evidence' }] }],
});
assert.equal(normalizedHotspots.schema, WORDPRESS_FUZZ_HOTSPOT_SUMMARY_SCHEMA);
assert.equal(normalizedHotspots.items[0].id, 'rest:/wp/v2/posts:GET /wp/v2/posts:query_count');
assert.equal(normalizedHotspots.items[0].metadata.surface_key, 'rest:/wp/v2/posts');
assert.equal(normalizedHotspots.items[0].metadata.operation_key, 'GET /wp/v2/posts');
assert.equal(normalizedHotspots.items[0].metric, 'query_count');
assert.equal(normalizedHotspots.items[0].evidence_refs[0], 'reports/query.json');

const codeboxObservations = normalizeFuzzObservationSet({
	id: 'codebox-native-measurements',
	queries: [{ case_id: 'case-1', target_id: 'rest:/wp/v2/posts', operation_id: 'GET /wp/v2/posts', query: 'SELECT * FROM wp_posts', count: 3, metric: 'query_count' }],
	timings: [{ case_id: 'case-1', target_id: 'rest:/wp/v2/posts', operation_id: 'GET /wp/v2/posts', subject: 'request', duration_ms: 42 }],
});
assert.equal(codeboxObservations.schema, WORDPRESS_FUZZ_OBSERVATION_SET_SCHEMA);
assert.equal(codeboxObservations.observations[0].family, 'query');
assert.equal(codeboxObservations.observations[0].metric, 'query_count');
assert.equal(codeboxObservations.observations[1].unit, 'ms');
const observationHotspots = fuzzHotspotSummaryFromObservationSet(codeboxObservations);
assert.equal(observationHotspots.schema, WORDPRESS_FUZZ_HOTSPOT_SUMMARY_SCHEMA);
assert.equal(observationHotspots.items[0].metadata.observation_id, codeboxObservations.observations[0].id);

const codeboxWordPressHotspots = normalizeFuzzHotspotSummary({
	schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
	db: [{ table: 'wp_posts', operation: 'SELECT', metric: 'query_count', count: 11 }],
	api: [{ route: '/wp-json/wp/v2/posts', method: 'GET', metric: 'duration_ms', duration_ms: 84 }],
});
assert.equal(codeboxWordPressHotspots.schema, WORDPRESS_FUZZ_HOTSPOT_SUMMARY_SCHEMA);
assert.equal(codeboxWordPressHotspots.items[0].dimension, 'database');
assert.equal(codeboxWordPressHotspots.items[0].metadata.surface_key, 'wp_posts');
assert.equal(codeboxWordPressHotspots.items[1].dimension, 'api');
assert.equal(codeboxWordPressHotspots.items[1].metadata.operation_key, 'GET');

const derivedAggregate = aggregateWordPressFuzzCoverage({
	artifacts: [
		{
			schema: 'homeboy/example-coverage-gap-report/v1',
			semantic_key: 'fuzz.coverage.gap_report',
			expected: 2,
			covered: 1,
			gaps: [{ id: 'route:/wp/v2/comments', type: 'rest_route', status: 'skipped' }],
		},
		{
			semantic_key: 'fuzz.hotspot.summary',
			hotspots: [{ surface: 'route:/wp/v2/posts', operation: 'GET /wp/v2/posts', metric: 'duration_ms', value: 77 }],
		},
		{
			name: 'wordpress-hotspots',
			payload: {
				schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
				db: [{ table: 'wp_options', operation: 'SELECT', metric: 'query_count', count: 5 }],
			},
		},
	],
});
assert.equal(derivedAggregate.coverage_gaps[0].id, 'route:/wp/v2/comments');
assert.equal(derivedAggregate.hotspot_summary.items[0].value, 77);
assert.equal(derivedAggregate.hotspot_summary.items.some((item) => item.metadata.surface_key === 'wp_options'), true);

Promise.all([
	runWpCodeboxFuzzSuite({
		taskId: 'unavailable-runtime',
		input: { id: 'unavailable-runtime', cases: [{ id: 'case-1' }] },
		publicCliCapabilities: { commands: { 'run-fuzz-suite': false, 'run-wordpress-workload': false } },
	}).then((summary) => {
		assert.equal(summary.schema, WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA);
		assert.equal(summary.status, 'skipped');
		assert.equal(summary.succeeded, false);
		assert.equal(summary.runtime_task_result.schema, 'homeboy/fuzz-runtime-task-result/v1');
		assert.equal(summary.runtime_task_result.provider.id, 'wp-codebox');
		assert.equal(summary.runtime_task_result.status, 'skipped');
		assert.equal(summary.failures[0].code, 'wp_codebox_fuzz_missing_runtime_contract_manifest');
		assert(summary.failures.some((failure) => failure.code === 'wp_codebox_fuzz_missing_public_cli_command'));
		assert.equal(summary.metadata.preflight.required.commands[0], 'run-fuzz-suite');
	}),

	runWpCodeboxFuzzSuite({
		taskId: 'declared-only-runtime',
		input: { id: 'declared-only-runtime', metadata: { declared_only: true } },
		runFuzzSuite: async () => ({
			json: {
				schema: 'wp-codebox/fuzz-suite-result/v1',
				request_id: 'declared-only-runtime',
				status: 'skipped',
				metadata: { readiness: { level: 'declared' } },
			},
		}),
	}).then((summary) => {
		assert.equal(summary.status, 'skipped');
		assert.equal(summary.failures.length, 0);
		assert.equal(summary.runtime_task_result.status, 'skipped');
	}),

	runWpCodeboxFuzzSuite({
		taskId: 'partial-artifact-runtime',
		input: { id: 'partial-artifact-runtime', cases: [{ id: 'case-1' }] },
		runFuzzSuite: async () => ({
			json: {
				schema: 'wp-codebox/fuzz-suite-result/v1',
				request_id: 'partial-artifact-runtime',
				status: 'succeeded',
				cases: [{ id: 'case-1', status: 'passed' }],
				artifacts: { fuzz_report: { path: 'reports/fuzz.json' } },
			},
		}),
	}).then((summary) => {
		assert.equal(summary.status, 'succeeded');
		assert.equal(summary.artifacts.length, 2);
		assert.equal(summary.artifacts[0].semantic_key, 'fuzz.report');
		assert.equal(summary.artifacts.some((artifact) => artifact.role === 'result_envelope'), true);
		assert.equal(summary.runtime_task_result.artifacts.length, 2);
		assert.equal(summary.runtime_task_result.artifacts[0].semantic_key, 'fuzz.report');
	}),

	runWpCodeboxFuzzSuite({
		taskId: 'successful-hotspot-runtime',
		input: { id: 'successful-hotspot-runtime', cases: [{ id: 'case-1' }] },
		runFuzzSuite: async () => ({
			json: {
				schema: 'wp-codebox/fuzz-suite-result/v1',
				request_id: 'successful-hotspot-runtime',
				status: 'succeeded',
				cases: [{ id: 'case-1', status: 'passed' }],
				artifacts: { fuzz_report: { path: 'reports/successful-hotspot-runtime.json' } },
				coverage: { exercised: [{ id: 'rest:/wp/v2/posts', type: 'rest_route' }] },
				hotspot_summary: {
					dimension: 'runtime',
					metric: 'duration_ms',
					unit: 'ms',
					items: [{ surface_key: 'rest:/wp/v2/posts', operation_key: 'GET /wp/v2/posts', value: 42, relative_score: 1, sample_count: 3 }],
				},
			},
		}),
	}).then((summary) => {
		assert.equal(summary.status, 'succeeded');
		assert.equal(summary.hotspot_summary.schema, WORDPRESS_FUZZ_HOTSPOT_SUMMARY_SCHEMA);
		assert.equal(summary.hotspot_summary.items[0].value, 42);
		assert.equal(summary.runtime_task_result.hotspot_summary.items[0].metric, 'duration_ms');
		const aggregate = aggregateWordPressFuzzCoverage({ artifacts: [summary.coverage], hotspot_summary: summary.hotspot_summary });
		assert.equal(aggregate.hotspot_summary.items[0].metadata.operation_key, 'GET /wp/v2/posts');
		assert.match(formatWordPressFuzzCoverageMarkdownReport(aggregate), /## Hotspots/);
		assert.match(formatWordPressFuzzCoverageMarkdownReport(aggregate), /duration_ms/);
	}),

	runWpCodeboxFuzzSuite({
		taskId: 'mutation-evidence-missing-runtime',
		input: {
			id: 'mutation-evidence-missing-runtime',
			cases: [{
				id: 'delete-post-case',
				metadata: { mutation_lifecycle: buildWordPressFuzzMutationLifecycleContract({ kind: 'rest', method: 'DELETE' }) },
			}],
		},
		runFuzzSuite: async () => ({
			json: {
				schema: 'wp-codebox/fuzz-suite-result/v1',
				request_id: 'mutation-evidence-missing-runtime',
				status: 'succeeded',
				cases: [{ id: 'delete-post-case', status: 'passed' }],
			},
		}),
	}).then((summary) => {
		assert.equal(summary.status, 'failed');
		assert.equal(summary.failures[0].code, 'wp_codebox_fuzz_mutation_lifecycle_evidence_missing');
		assert(summary.failures[0].missing_evidence.some((entry) => entry.kind === 'delete-boundary'));
	}),
]).then(() => {
	console.log('WordPress fuzz runtime task contract tests passed.');
});
