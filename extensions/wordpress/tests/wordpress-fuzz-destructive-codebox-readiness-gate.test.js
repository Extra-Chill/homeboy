'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const path = require('node:path');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(
	__dirname,
	'..',
	'..',
	'tests',
	'fixtures',
	'wp-codebox-core-runtime-contract.cjs'
);

/**
 * Internal dependencies
 */
const {
	WP_CODEBOX_FUZZ_PREFLIGHT_SCHEMA,
	normalizeWpCodeboxDestructiveReadiness,
	preflightWpCodeboxFuzzCapabilityContract,
	wpCodeboxFuzzSuiteTaskRequest,
} = require('../lib/wp-codebox-fuzz-run');
const {
	runWordPressFuzzRunnerResult,
} = require('../lib/wordpress-fuzz-runner');

const runtimeContractManifest = {
	schema: 'wp-codebox/runtime-contract-manifest/v1',
	version: 1,
	schemas: {
		wordpressRuntime: {
			workloadRun: 'wp-codebox/wordpress-workload-run/v1',
			fuzzSuite: 'wp-codebox/fuzz-suite/v1',
			fuzzSuiteResult: 'wp-codebox/fuzz-suite-result/v1',
		},
	},
	abilities: {
		wordpressRuntime: {
			runWorkload: 'wp-codebox/run-wordpress-workload',
			runFuzzSuite: 'wp-codebox/run-fuzz-suite',
		},
	},
};

const destructivePlan = {
	schema: 'wordpress-fuzz-plan/v1',
	id: 'generic-destructive-plan',
	metadata: { mutation_mode: 'aggressive-isolated' },
	targets: [{
		id: 'posts-delete',
		surface_id: 'route:/wp/v2/posts/(?P<id>)',
		cases: [{
			id: 'delete-post',
			method: 'DELETE',
			path: '/wp/v2/posts/123',
			destructive_reasons: ['rest_method_mutates_state'],
			metadata: { safety: { level: 'destructive', mutates: true, rollback_required: true } },
		}],
	}],
};

const destructiveRequest = wpCodeboxFuzzSuiteTaskRequest({
	taskId: 'destructive-codebox-readiness-gate',
	input: {
		id: 'destructive-codebox-readiness-suite',
		homeboy_fuzz_workload: { id: 'destructive-workload', plan: destructivePlan },
		cases: [{
			id: 'delete-post',
			method: 'DELETE',
			path: '/wp/v2/posts/123',
			destructive_reasons: ['rest_method_mutates_state'],
			metadata: { safety: { level: 'destructive', mutates: true } },
		}],
	},
});

const incompleteReadiness = {
	schema: 'wp-codebox/fuzz-runner-readiness/v1',
	status: 'ready',
	mode: 'runtime-backed',
	entrypoint: 'run-fuzz-suite --runner-mode=runtime-backed',
	capabilities: {
		commands: ['wordpress.rest-request', 'wordpress.run-workload'],
		runtimeActionTypes: ['rest_request'],
	},
	unsupportedRequiredCapabilities: [],
};

const blockedPreflight = preflightWpCodeboxFuzzCapabilityContract({
	request: destructiveRequest,
	runtimeContractManifest,
	publicCliReadiness: incompleteReadiness,
});

assert.equal(blockedPreflight.schema, WP_CODEBOX_FUZZ_PREFLIGHT_SCHEMA);
assert.equal(blockedPreflight.ok, false);
assert.equal(blockedPreflight.destructive_readiness.required, true);
assert.deepEqual(
	blockedPreflight.destructive_readiness.missing_primitives.map((primitive) => primitive.key),
	['runtime_isolation', 'rollback_checkpoint', 'rollback_restore', 'external_http_guardrail', 'artifact_export']
);
assert.equal(blockedPreflight.missing_contracts.some((contract) => contract.type === 'destructive_readiness'), true);
assert.equal(blockedPreflight.diagnostics.some((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_destructive_readiness'), true);
assert.match(blockedPreflight.diagnostics.find((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_destructive_readiness').message, /external HTTP guardrail/);

const missingReadinessCliCalls = [];
const missingReadinessPreflight = preflightWpCodeboxFuzzCapabilityContract({
	request: destructiveRequest,
	runtimeContractManifest,
	runPublicCli: ({ args }) => {
		missingReadinessCliCalls.push(args);
		assert.deepEqual(args, ['fuzz', 'readiness', '--format=json']);
		return { status: 1, stdout: '', stderr: 'readiness unavailable' };
	},
});

assert.equal(missingReadinessPreflight.ok, false);
assert.deepEqual(missingReadinessCliCalls, [['fuzz', 'readiness', '--format=json']]);
assert.equal(missingReadinessPreflight.capabilities.commands['run-fuzz-suite'], false);
assert.equal(missingReadinessPreflight.capabilities.commands['run-wordpress-workload'], false);
assert.equal(missingReadinessPreflight.missing_contracts.some((contract) => contract.type === 'public_cli_readiness_command'), true);
assert.equal(missingReadinessPreflight.diagnostics.some((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_public_cli_readiness_command'), true);

const missingManifestPreflight = preflightWpCodeboxFuzzCapabilityContract({
	request: destructiveRequest,
	runtimeContractManifest: {},
	publicCliReadiness: incompleteReadiness,
});

assert.equal(missingManifestPreflight.ok, false);
assert.equal(missingManifestPreflight.missing_contracts.some((contract) => contract.type === 'runtime_contract_manifest'), true);

const completeReadiness = {
	...incompleteReadiness,
	isolation: { runtime_backed: true, snapshot: true, restore: true, reset: true },
	rollback: { checkpoint: true, restore: true },
	guardrails: { external_http_guardrail: true },
	artifacts: { export: true },
	capabilities: {
		...incompleteReadiness.capabilities,
		capabilities: ['snapshot', 'checkpoint', 'restore', 'reset', 'runtime-backed-isolation', 'external-http-guardrail', 'artifact-export'],
	},
};

const passedPreflight = preflightWpCodeboxFuzzCapabilityContract({
	request: destructiveRequest,
	runtimeContractManifest,
	publicCliReadiness: completeReadiness,
});

assert.equal(passedPreflight.ok, true);
assert.equal(passedPreflight.destructive_readiness.ok, true);
assert.equal(passedPreflight.destructive_readiness.facts.runtime_isolation, true);
assert.equal(passedPreflight.destructive_readiness.facts.rollback_checkpoint, true);
assert.equal(passedPreflight.destructive_readiness.facts.rollback_restore, true);
assert.equal(passedPreflight.destructive_readiness.facts.external_http_guardrail, true);
assert.equal(passedPreflight.destructive_readiness.facts.artifact_export, true);
assert.equal(passedPreflight.capabilities.capabilities.includes('runtime-isolation'), true);
assert.equal(passedPreflight.capabilities.capabilities.includes('external-http-guardrail'), true);
assert.equal(passedPreflight.capabilities.capabilities.includes('artifact-export'), true);

assert.deepEqual(normalizeWpCodeboxDestructiveReadiness(completeReadiness, {
	request: destructiveRequest,
	suiteInput: destructiveRequest.executor.config.runtime_task.input,
	plan: destructivePlan,
}).missing_primitives, undefined);

(async () => {
	const result = await runWordPressFuzzRunnerResult({
		env: {
			workloadPath: '/unused/destructive-workload.json',
			workloadId: 'destructive-workload',
			runId: 'destructive-run',
		},
		workload: {
			id: 'destructive-workload',
			plan: destructivePlan,
		},
		runtimeContractManifest,
		publicCliReadiness: incompleteReadiness,
	});

	assert.equal(result.status, 'skipped');
	assert.equal(result.succeeded, false);
	assert.equal(result.wp_codebox_result.metadata.preflight.destructive_readiness.required, true);
	assert.equal(result.wp_codebox_result.metadata.preflight.destructive_readiness.ok, false);
	assert.equal(result.homeboy_fuzz_campaign.safety_class, 'destructive');
	assert.equal(result.homeboy_fuzz_campaign.metadata.diagnostics.some((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_destructive_readiness'), true);
	assert.equal(result.homeboy_fuzz_result_envelope.gates.failures.some((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_destructive_readiness'), true);

	const blockedPrecomputed = await runWordPressFuzzRunnerResult({
		env: {
			workloadPath: '/unused/precomputed-destructive-workload.json',
			workloadId: 'precomputed-destructive-workload',
			runId: 'precomputed-destructive-run',
		},
		workload: {
			id: 'precomputed-destructive-workload',
			plan: destructivePlan,
			wp_codebox_suite_result: {
				schema: 'wp-codebox/fuzz-suite-result/v1',
				request_id: 'precomputed-destructive-run',
				status: 'succeeded',
			},
		},
	});

	assert.equal(blockedPrecomputed.status, 'unsupported');
	assert.equal(blockedPrecomputed.succeeded, false);
	assert.equal(blockedPrecomputed.wp_codebox_result.metadata.precomputed_result_blocked, true);
	assert.equal(blockedPrecomputed.wp_codebox_result.failures.some((diagnostic) => diagnostic.code === 'wp_codebox_precomputed_fuzz_result_not_fixture_only'), true);

	const fixturePrecomputed = await runWordPressFuzzRunnerResult({
		env: {
			workloadPath: '/unused/fixture-only-destructive-workload.json',
			workloadId: 'fixture-only-destructive-workload',
			runId: 'fixture-only-destructive-run',
		},
		workload: {
			id: 'fixture-only-destructive-workload',
			fixture_only: true,
			plan: destructivePlan,
			wp_codebox_suite_result: {
				schema: 'wp-codebox/fuzz-suite-result/v1',
				request_id: 'fixture-only-destructive-run',
				status: 'succeeded',
			},
		},
	});

	assert.equal(fixturePrecomputed.status, 'succeeded');
	assert.equal(fixturePrecomputed.succeeded, true);
})().catch((error) => {
	process.nextTick(() => {
		throw error;
	});
});
