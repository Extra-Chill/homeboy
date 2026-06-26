'use strict';

const assert = require('node:assert/strict');
const {
	FUZZ_CASE_INTENT_SCHEMA,
	FUZZ_WORKLOAD_SCHEMA,
	assertFuzzReadinessMetadata,
	assertRunnerNeutralFuzzCaseIntent,
	collectGenericFuzzWorkloadIssues,
} = require('../lib/wordpress-fuzz-manifest-validator');

function fuzzWorkload(overrides = {}) {
	return {
		schema: FUZZ_WORKLOAD_SCHEMA,
		id: 'product-fuzz',
		label: 'Product fuzz',
		safety_class: 'read_only',
		metadata: {
			readiness: {
				level: 'proven',
				coverage_contract: 'Product route coverage with durable artifacts.',
				proof_refs: ['https://github.com/example/product/pull/1'],
				proof_bundle: {
					artifact_refs: ['artifact:product-fuzz/report.json'],
					run_ids: ['homeboy-runs:product-fuzz-1'],
					gap_reports: ['gh:example/product#1'],
					fuzz_result_artifacts: ['report'],
				},
			},
		},
		target: { type: 'wordpress-plugin', slug: 'product' },
		workload: { runner: 'wp-codebox', type: 'json', path: '${package.root}/bench/product.workload.json' },
		cases: [
			{
				case_id: 'product-fuzz:default',
				intent: {
					schema: FUZZ_CASE_INTENT_SCHEMA,
					type: 'wordpress-plugin-workload',
					plugin: { activation: 'product/product.php' },
					execute: {
						workload_ref: 'default',
						path: '${package.root}/bench/product.workload.json',
						type: 'json',
					},
					collect: [{ artifact: 'report' }],
				},
				artifacts: [{ name: 'report', path: 'report.json', required: true }],
				metadata: { safety_class: 'read_only' },
			},
		],
		artifacts: { expected: [{ name: 'report', path: 'report.json', required: true }] },
		...overrides,
	};
}

const workload = fuzzWorkload();

assert.deepEqual(collectGenericFuzzWorkloadIssues(workload), []);
assert.equal(assertFuzzReadinessMetadata(workload).level, 'proven');
assert.equal(assertRunnerNeutralFuzzCaseIntent(workload, workload.cases[0]).schema, FUZZ_CASE_INTENT_SCHEMA);

assert.match(
	collectGenericFuzzWorkloadIssues(fuzzWorkload({ safety_class: 'unknown' }))[0],
	/safety_class must be one of read_only, idempotent, isolated_mutation, destructive/
);

assert.throws(
	() => assertFuzzReadinessMetadata(fuzzWorkload({
		metadata: { readiness: { level: 'proven', coverage_contract: 'Local proof.', proof_refs: ['http://localhost:8888/proof'] } },
	})),
	/proof_refs entries must be reviewer-facing refs/
);

assert.throws(
	() => assertFuzzReadinessMetadata(fuzzWorkload({
		metadata: {
			readiness: {
				level: 'proven',
				coverage_contract: 'Unknown artifact proof.',
				proof_refs: ['https://github.com/example/product/pull/1'],
				proof_bundle: {
					artifact_refs: ['artifact:product-fuzz/report.json'],
					run_ids: ['homeboy-runs:product-fuzz-1'],
					gap_reports: ['gh:example/product#1'],
					fuzz_result_artifacts: ['missing-report'],
				},
			},
		},
	})),
	/must name a required case or expected artifact/
);

assert.match(
	collectGenericFuzzWorkloadIssues(fuzzWorkload({
		cases: [
			{
				...workload.cases[0],
				phases: { action: [{ command: 'wordpress.run-workload' }] },
			},
		],
	}))[0],
	/runner-neutral case intent must not embed runner command phases/
);

console.log('wordpress fuzz manifest validator smoke passed');
