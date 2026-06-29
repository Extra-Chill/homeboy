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
	normalizeWpCodeboxFuzzSuiteResult,
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
	['disposable_runtime', 'disposable_sandbox_boundary', 'destructive_permission', 'mutation_boundary', 'external_side_effect_guardrail', 'artifact_export', 'teardown_discard']
);
assert.equal(blockedPreflight.missing_contracts.some((contract) => contract.type === 'destructive_readiness'), true);
assert.equal(blockedPreflight.diagnostics.some((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_destructive_readiness'), true);
assert.match(blockedPreflight.diagnostics.find((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_destructive_readiness').message, /disposable sandbox boundary identity/);

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
	disposable: true,
	isolation: { runtime_backed: true, disposable: true },
	guardrails: { external_side_effect_guardrail: true },
	artifacts: { export: true },
	destructiveModeRequirements: {
		supported: true,
		destructiveMutationIntent: 'destructive',
		requiredSandboxBoundary: { disposable: true, destructivePermission: true, teardown: 'discard' },
		requiredArtifacts: ['mutation-isolation-artifact', 'delete-boundary-artifact'],
		deleteBoundaryCapability: 'delete-boundary-artifact',
		rawDeleteCapability: null,
	},
	capabilities: {
		...incompleteReadiness.capabilities,
		capabilities: ['disposable-runtime', 'runtime-backed-isolation', 'external-side-effect-guardrail', 'artifact-export', 'wordpress-runtime:sandbox-isolation-proof'],
	},
};

const passedPreflight = preflightWpCodeboxFuzzCapabilityContract({
	request: destructiveRequest,
	runtimeContractManifest,
	publicCliReadiness: completeReadiness,
});

assert.equal(passedPreflight.ok, true);
assert.equal(passedPreflight.destructive_readiness.ok, true);
assert.equal(passedPreflight.destructive_readiness.facts.disposable_runtime, true);
assert.equal(passedPreflight.destructive_readiness.facts.disposable_sandbox_boundary, true);
assert.equal(passedPreflight.destructive_readiness.facts.destructive_permission, true);
assert.equal(passedPreflight.destructive_readiness.facts.mutation_boundary, true);
assert.equal(passedPreflight.destructive_readiness.facts.external_side_effect_guardrail, true);
assert.equal(passedPreflight.destructive_readiness.facts.artifact_export, true);
assert.equal(passedPreflight.destructive_readiness.facts.teardown_discard, true);
assert.equal(passedPreflight.capabilities.capabilities.includes('disposable-runtime'), true);
assert.equal(passedPreflight.capabilities.capabilities.includes('runtime-isolation'), true);
assert.equal(passedPreflight.capabilities.capabilities.includes('disposable-sandbox-boundary'), true);
assert.equal(passedPreflight.capabilities.capabilities.includes('destructive-permission'), true);
assert.equal(passedPreflight.capabilities.capabilities.includes('mutation-isolation-artifact'), true);
assert.equal(passedPreflight.capabilities.capabilities.includes('delete-boundary-artifact'), true);
assert.equal(passedPreflight.capabilities.capabilities.includes('sandbox-isolation-proof'), true);
assert.equal(passedPreflight.capabilities.capabilities.includes('external-side-effect-guardrail'), true);
assert.equal(passedPreflight.capabilities.capabilities.includes('artifact-export'), true);
assert.deepEqual(
	destructiveRequest.executor.config.runtime_task.input.metadata.disposableSandboxBoundary,
	{ disposable: true, destructivePermission: true, teardown: 'discard', backend: 'wordpress-playground', environment: 'wordpress', hostAccess: 'declared-mounts-only' }
);

const missingDisposableArtifacts = normalizeWpCodeboxFuzzSuiteResult({
	schema: 'wp-codebox/fuzz-suite-result/v1',
	request_id: 'destructive-artifact-missing',
	status: 'succeeded',
	wordpress_fuzz_result: {
		schema: 'wordpress-fuzz-result/v1',
		status: 'succeeded',
		cases: [{ id: 'delete-post', status: 'passed' }],
	},
	artifacts: [{ name: 'case-log', path: 'files/case-log.jsonl' }],
}, { request: destructiveRequest });

assert.equal(missingDisposableArtifacts.status, 'failed');
assert.equal(missingDisposableArtifacts.failures.some((failure) => failure.code === 'wp_codebox_fuzz_disposable_lifecycle_artifacts_missing'), true);

const passedDisposableArtifacts = normalizeWpCodeboxFuzzSuiteResult({
	schema: 'wp-codebox/fuzz-suite-result/v1',
	request_id: 'destructive-artifact-passed',
	status: 'succeeded',
	wordpress_fuzz_result: {
		schema: 'wordpress-fuzz-result/v1',
		status: 'succeeded',
		cases: [{ id: 'delete-post', status: 'passed' }],
	},
	artifacts: [
		{ name: 'sandbox-isolation-proof', kind: 'sandbox-isolation-proof', schema: 'wp-codebox/sandbox-isolation-proof/v1', path: 'files/sandbox-isolation/delete-post-proof.json' },
		{ name: 'delete-boundary-artifact', kind: 'delete-boundary-artifact', schema: 'wp-codebox/delete-boundary-artifact/v1', path: 'files/delete-boundaries/delete-post.json' },
	],
}, { request: destructiveRequest });

assert.equal(passedDisposableArtifacts.status, 'succeeded');
assert.equal(passedDisposableArtifacts.failures.some((failure) => failure.code === 'wp_codebox_fuzz_disposable_lifecycle_artifacts_missing'), false);

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
