'use strict';

const assert = require('node:assert/strict');

const {
  normalizeAgentTaskOutcome,
  normalizeAgentTaskStatus,
  normalizeProviderTaskOutcome,
  normalizeProviderStatus,
  providerFailureClassification,
} = require('../../agent-runtimes/wp-codebox/lib/provider-outcome-normalizer');

const request = { task_id: 'provider-task-123' };

const legacyProviderError = normalizeProviderTaskOutcome(request, {
  success: false,
  provider_error: true,
  summary: 'Provider failed before runtime startup.',
  artifacts: { bundle: { id: 'bundle-1', directory: '/tmp/provider-artifacts' } },
  diagnostics: [{ kind: 'provider.auth', message: 'OAuth refresh failed.', data: { provider: 'example' } }],
}, {
  provider: 'example.provider',
  integrationContract: 'example/provider-task/v1',
});

assert.equal(legacyProviderError.schema, 'homeboy/agent-task-outcome/v1');
assert.equal(legacyProviderError.task_id, 'provider-task-123');
assert.equal(legacyProviderError.status, 'provider_error');
assert.equal(legacyProviderError.failure_classification, 'provider');
assert.equal(legacyProviderError.failure_category, 'provider.auth');
assert.equal(legacyProviderError.retryable, false);
assert.equal(legacyProviderError.summary, 'Provider failed before runtime startup.');
assert.equal(legacyProviderError.artifacts[0].id, 'bundle-1');
assert.equal(legacyProviderError.artifacts[0].path, '/tmp/provider-artifacts');
assert.equal(legacyProviderError.diagnostics[0].class, 'provider.auth');
assert.equal(legacyProviderError.metadata.provider, 'example.provider');
assert.equal(legacyProviderError.metadata.integration_contract, 'example/provider-task/v1');

const completedWithNonZeroExit = normalizeProviderTaskOutcome(request, {
  success: true,
  status: 'completed',
  outputs: { issue_url: 'https://github.com/example/repo/issues/12' },
  evidence_refs: [{ kind: 'issue', uri: 'https://github.com/example/repo/issues/12', label: 'Issue' }],
}, { exitStatus: 1 });

assert.equal(completedWithNonZeroExit.status, 'failed');
assert.equal(completedWithNonZeroExit.failure_classification, 'execution_failed');
assert.equal(completedWithNonZeroExit.failure_category, 'runtime.execution_failed');
assert.equal(completedWithNonZeroExit.retryable, false);
assert.equal(completedWithNonZeroExit.outputs.issue_url, 'https://github.com/example/repo/issues/12');
assert.equal(completedWithNonZeroExit.evidence_refs[0].kind, 'issue');

assert.equal(normalizeProviderStatus({ success: true, status: 'completed' }, 1), 'failed');
assert.equal(normalizeProviderStatus({ success: true, status: 'succeeded' }, 1), 'failed');

const normalizedRuntimeFailure = normalizeProviderTaskOutcome(request, {
  success: false,
  status: 'failed',
  failure_classification: 'runtime',
  diagnostics: [{ code: 'runtime.no_session', message: 'Runtime failed before creating a session.' }],
});

assert.equal(normalizedRuntimeFailure.status, 'failed');
assert.equal(normalizedRuntimeFailure.failure_classification, 'execution_failed');
assert.equal(normalizedRuntimeFailure.diagnostics[0].class, 'runtime.no_session');

const failedStatusWithoutSuccess = normalizeAgentTaskOutcome(request, {
  status: 'failed',
  summary: 'Provider reported a terminal failure without success false.',
}, { status: 'succeeded' });
assert.equal(failedStatusWithoutSuccess.status, 'failed');
assert.equal(failedStatusWithoutSuccess.failure_classification, 'execution_failed');

const providerErrorWithoutSuccess = normalizeAgentTaskOutcome(request, {
  status: 'provider_error',
  summary: 'Provider failed before runtime startup.',
});
assert.equal(providerErrorWithoutSuccess.status, 'provider_error');
assert.equal(providerErrorWithoutSuccess.failure_classification, 'provider');

const timeoutWithoutSuccess = normalizeAgentTaskOutcome(request, {
  timeout: true,
  summary: 'Timed out.',
});
assert.equal(timeoutWithoutSuccess.status, 'timeout');
assert.equal(timeoutWithoutSuccess.failure_classification, 'timeout');
assert.equal(timeoutWithoutSuccess.failure_category, 'runtime.timeout');
assert.equal(timeoutWithoutSuccess.retryable, true);

const emptyJsonOutcome = normalizeAgentTaskOutcome(request, {});
assert.equal(emptyJsonOutcome.status, 'failed');
assert.equal(emptyJsonOutcome.failure_classification, 'execution_failed');

const noOpOutcome = normalizeAgentTaskOutcome(request, { success: true, outcome: 'no_op' });
assert.equal(noOpOutcome.status, 'no_op');
assert.equal(noOpOutcome.failure_classification, undefined);
assert.equal(noOpOutcome.failure_category, undefined);
assert.equal(noOpOutcome.retryable, undefined);

assert.equal(normalizeAgentTaskStatus({ status: 'completed' }), 'succeeded');
assert.equal(normalizeAgentTaskStatus({ success: false }), 'failed');

assert.equal(normalizeProviderStatus({ success: true, outcome: 'no_op' }), 'no_op');
assert.equal(providerFailureClassification('task', 'failed'), 'execution_failed');
assert.equal(providerFailureClassification('incomplete', 'failed'), 'execution_failed');
assert.equal(providerFailureClassification('max_turns', 'timeout'), 'timeout');
assert.equal(providerFailureClassification('custom-runtime-detail', 'failed'), 'unknown');
assert.throws(() => normalizeProviderTaskOutcome({}, {}), /request.task_id/);

console.log('✓ provider outcome normalizer boundary test PASSED');
