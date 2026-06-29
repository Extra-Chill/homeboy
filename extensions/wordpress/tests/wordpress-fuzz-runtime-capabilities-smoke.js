'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_FUZZ_MUTATION_POLICY_SCHEMA,
	WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA,
	gateWordPressFuzzCaseForMutationPolicy,
	gateWordPressFuzzCaseForRuntimeCapabilities,
	normalizeWordPressFuzzExecutionTier,
	normalizeWordPressFuzzMutationMode,
	normalizeWordPressFuzzRuntimeCapabilities,
	normalizeWordPressFuzzRuntimeCapability,
	requiredCapabilitiesForWordPressFuzzCase,
} = require('../lib/wordpress-fuzz-runtime-capabilities');

assert.equal(normalizeWordPressFuzzRuntimeCapability('db-transaction'), 'transaction');
assert.equal(normalizeWordPressFuzzRuntimeCapability('database_reset'), 'reset');
assert.equal(normalizeWordPressFuzzRuntimeCapability('rollback-safe-rest'), 'rest-rollback');
assert.equal(normalizeWordPressFuzzRuntimeCapability('disposable_boundary'), 'disposable-sandbox-boundary');
assert.equal(normalizeWordPressFuzzRuntimeCapability('destructive_permission'), 'destructive-permission');
assert.equal(normalizeWordPressFuzzRuntimeCapability('mutation_boundary'), 'mutation-isolation-artifact');
assert.equal(normalizeWordPressFuzzRuntimeCapability('external_side_effect_guardrail'), 'external-side-effect-guardrail');
assert.equal(normalizeWordPressFuzzRuntimeCapability('unknown'), '');
assert.equal(normalizeWordPressFuzzMutationMode('destructive_deny'), 'destructive-deny');
assert.equal(normalizeWordPressFuzzMutationMode('read-only'), 'read_only');
assert.equal(normalizeWordPressFuzzExecutionTier('read-only-executable'), 'read_only_executable');
assert.equal(normalizeWordPressFuzzExecutionTier('', { discovered: true }), 'discovered');
assert.equal(normalizeWordPressFuzzExecutionTier('', { executable: false }), 'plan_only');
assert.equal(normalizeWordPressFuzzExecutionTier('', { executable: true, mutates: true }), 'isolated_mutating_executable');

const contract = normalizeWordPressFuzzRuntimeCapabilities({
	capabilities: ['snapshots', 'rollback', 'reset-db', 'crud-execution'],
	execution: { rest: true },
	metadata: { runtime: 'fixture' },
});
assert.equal(contract.schema, WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA);
assert.deepEqual(contract.capabilities, ['crud', 'reset', 'restore', 'snapshot']);
assert.equal(contract.isolation.snapshot, true);
assert.equal(contract.isolation.restore, true);
assert.equal(contract.isolation.reset, true);
assert.equal(contract.execution.crud, true);
assert.equal(contract.execution.rest, true);
assert.deepEqual(contract.metadata, { runtime: 'fixture' });

const disposableMutationCapabilities = ['crud', 'disposable-runtime', 'disposable-sandbox-boundary', 'destructive-permission', 'mutation-isolation-artifact', 'sandbox-isolation-proof'];
assert.deepEqual(requiredCapabilitiesForWordPressFuzzCase('mutating_crud'), disposableMutationCapabilities);
assert.deepEqual(requiredCapabilitiesForWordPressFuzzCase('mutating_rest'), ['rest', 'disposable-runtime', 'disposable-sandbox-boundary', 'destructive-permission', 'mutation-isolation-artifact', 'sandbox-isolation-proof']);

const skipped = gateWordPressFuzzCaseForRuntimeCapabilities({
	id: 'case-1',
	skip_reasons: ['explicit-opt-in-required'],
	metadata: { planned: true },
}, { capabilities: ['crud'] }, { required_capabilities: disposableMutationCapabilities });
assert.equal(skipped.executable, false);
assert.equal(skipped.execution_tier, 'plan_only');
assert.deepEqual(skipped.required_capabilities, disposableMutationCapabilities.sort());
assert.deepEqual(skipped.metadata.missing_capabilities, ['destructive-permission', 'disposable-runtime', 'disposable-sandbox-boundary', 'mutation-isolation-artifact', 'sandbox-isolation-proof']);
assert.deepEqual(skipped.skip_reasons, ['explicit-opt-in-required', 'missing-runtime-fuzz-capabilities']);

const executable = gateWordPressFuzzCaseForRuntimeCapabilities({
	id: 'case-2',
	skip_reasons: [],
	metadata: {},
}, { capabilities: disposableMutationCapabilities }, { required_capabilities: disposableMutationCapabilities });
assert.equal(executable.executable, true);
assert.equal(executable.execution_tier, 'read_only_executable');
assert.equal(executable.metadata.executable, true);
assert.equal(executable.metadata.gated, false);
assert.equal(executable.metadata.runtime_capability_gated, false);

const restMutationExecutable = gateWordPressFuzzCaseForRuntimeCapabilities({
	id: 'case-rest',
	skip_reasons: [],
	metadata: { safety: { mutates: true } },
}, { capabilities: ['rest', 'disposable-runtime', 'disposable-sandbox-boundary', 'destructive-permission', 'mutation-isolation-artifact', 'sandbox-isolation-proof'] }, {
	required_capabilities: ['rest', 'disposable-runtime', 'disposable-sandbox-boundary', 'destructive-permission', 'mutation-isolation-artifact', 'sandbox-isolation-proof'],
	mutation_mode: 'isolated',
	mutates: true,
});
assert.equal(restMutationExecutable.executable, true);
assert.equal(restMutationExecutable.execution_tier, 'isolated_mutating_executable');
assert.equal(restMutationExecutable.metadata.required_any_capabilities, undefined);

const explicitAnyCapabilityMissing = gateWordPressFuzzCaseForRuntimeCapabilities({
	id: 'case-explicit-any-missing',
	skip_reasons: [],
	metadata: { safety: { mutates: true } },
}, { capabilities: ['rest', 'disposable-runtime', 'disposable-sandbox-boundary', 'destructive-permission', 'mutation-isolation-artifact', 'sandbox-isolation-proof'] }, {
	required_capabilities: ['rest', 'disposable-runtime', 'disposable-sandbox-boundary', 'destructive-permission', 'mutation-isolation-artifact', 'sandbox-isolation-proof'],
	required_any_capabilities: [['restore', 'reset']],
	mutation_mode: 'isolated',
	mutates: true,
});
assert.equal(explicitAnyCapabilityMissing.executable, false);
assert.deepEqual(explicitAnyCapabilityMissing.metadata.missing_any_capabilities, [['reset', 'restore']]);

const restMutationMissingDisposableBoundary = gateWordPressFuzzCaseForRuntimeCapabilities({
	id: 'case-rest-missing',
	skip_reasons: [],
	metadata: { safety: { mutates: true } },
}, { capabilities: ['rest', 'disposable-runtime'] }, {
	required_capabilities: ['rest', 'disposable-runtime', 'disposable-sandbox-boundary', 'destructive-permission', 'mutation-isolation-artifact', 'sandbox-isolation-proof'],
	mutation_mode: 'isolated',
	mutates: true,
});
assert.equal(restMutationMissingDisposableBoundary.executable, false);
assert.deepEqual(restMutationMissingDisposableBoundary.metadata.missing_capabilities, ['destructive-permission', 'disposable-sandbox-boundary', 'mutation-isolation-artifact', 'sandbox-isolation-proof']);

const policyDenied = gateWordPressFuzzCaseForMutationPolicy({
	id: 'case-3',
	destructive_reasons: ['rest_method_mutates_state'],
	metadata: { planned: true },
}, { mutation_mode: 'read_only' });
assert.equal(policyDenied.executable, false);
assert.equal(policyDenied.execution_tier, 'plan_only');
assert.equal(policyDenied.metadata.mutation_policy.schema, WORDPRESS_FUZZ_MUTATION_POLICY_SCHEMA);
assert.equal(policyDenied.metadata.mutation_policy.mode, 'read_only');
assert.deepEqual(policyDenied.skip_reasons, ['mutation-policy-read-only']);

const capabilityAndPolicyDenied = gateWordPressFuzzCaseForRuntimeCapabilities({
	id: 'case-4',
	destructive_reasons: ['db-mutation'],
	metadata: {},
}, { capabilities: ['database', 'snapshot', 'transaction', 'reset'] }, {
	required_capabilities: ['database', 'snapshot', 'transaction', 'reset'],
	mutation_mode: 'destructive-deny',
	mutates: true,
});
assert.equal(capabilityAndPolicyDenied.executable, false);
assert.equal(capabilityAndPolicyDenied.execution_tier, 'plan_only');
assert.equal(capabilityAndPolicyDenied.metadata.runtime_capability_gated, undefined);
assert.equal(capabilityAndPolicyDenied.metadata.mutation_policy_gated, true);
assert.deepEqual(capabilityAndPolicyDenied.skip_reasons, ['mutation-policy-destructive-deny']);

console.log('WordPress fuzz runtime capabilities smoke passed.');
