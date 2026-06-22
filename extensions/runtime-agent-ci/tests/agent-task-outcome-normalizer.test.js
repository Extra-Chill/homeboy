'use strict';

const assert = require('node:assert/strict');

const {
  agentTaskFailureCategory,
  agentTaskFailureRetryable,
  normalizeAgentTaskOutcome,
  normalizeAgentTaskStatus,
  normalizeProviderTaskOutcome,
  normalizeProviderStatus,
  providerFailureClassification,
} = require('../lib/agent-task-outcome-normalizer');

const request = { task_id: 'provider-task-123' };

const cases = [
  {
    name: 'provider auth is terminal and not retryable',
    result: {
      success: false,
      provider_error: true,
      summary: 'OAuth refresh failed before runtime startup.',
      diagnostics: [{ kind: 'provider.auth', message: 'OAuth refresh failed.', data: { provider: 'example' } }],
    },
    expected: { status: 'provider_error', classification: 'provider', category: 'provider.auth', retryable: false },
  },
  {
    name: 'provider rate limits are retryable',
    result: {
      success: false,
      provider_error: true,
      diagnostics: [{ code: 'provider.rate_limit', message: 'HTTP 429 rate limit exceeded.' }],
    },
    expected: { status: 'provider_error', classification: 'provider', category: 'provider.rate_limit', retryable: true },
  },
  {
    name: 'provider quota is terminal and not retryable',
    result: {
      success: false,
      provider_error: true,
      diagnostics: [{ code: 'provider.quota', message: 'Insufficient quota or billing credits.' }],
    },
    expected: { status: 'provider_error', classification: 'provider', category: 'provider.quota', retryable: false },
  },
  {
    name: 'provider model errors are terminal and not retryable',
    result: {
      success: false,
      provider_error: true,
      diagnostics: [{ code: 'provider.model', message: 'Unknown model deployment.' }],
    },
    expected: { status: 'provider_error', classification: 'provider', category: 'provider.model', retryable: false },
  },
  {
    name: 'runtime transient failures are retryable',
    result: {
      success: false,
      failure_classification: 'transient',
      diagnostics: [{ code: 'runtime.network', message: 'Temporary connection reset.' }],
    },
    expected: { status: 'failed', classification: 'transient', category: 'runtime.transient', retryable: true },
  },
  {
    name: 'runtime execution failures are terminal by default',
    result: {
      success: false,
      failure_classification: 'runtime',
      diagnostics: [{ code: 'runtime.no_session', message: 'Runtime failed before creating a session.' }],
    },
    expected: { status: 'failed', classification: 'execution_failed', category: 'runtime.execution_failed', retryable: false },
  },
  {
    name: 'timeouts are retryable runtime failures',
    result: {
      timeout: true,
      summary: 'Timed out.',
    },
    expected: { status: 'timeout', classification: 'timeout', category: 'runtime.timeout', retryable: true },
  },
];

for (const item of cases) {
  const outcome = normalizeAgentTaskOutcome(request, item.result);
  assert.equal(outcome.status, item.expected.status, item.name);
  assert.equal(outcome.failure_classification, item.expected.classification, item.name);
  assert.equal(outcome.failure_category, item.expected.category, item.name);
  assert.equal(outcome.retryable, item.expected.retryable, item.name);
}

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

const noOpOutcome = normalizeAgentTaskOutcome(request, { success: true, outcome: 'no_op' });
assert.equal(noOpOutcome.status, 'no_op');
assert.equal(noOpOutcome.failure_classification, undefined);
assert.equal(noOpOutcome.failure_category, undefined);
assert.equal(noOpOutcome.retryable, undefined);

assert.equal(normalizeAgentTaskStatus({ status: 'completed' }), 'succeeded');
assert.equal(normalizeAgentTaskStatus({ success: false }), 'failed');
assert.equal(normalizeProviderStatus({ success: true, status: 'completed' }, 1), 'failed');
assert.equal(normalizeProviderStatus({ success: true, status: 'succeeded' }, 1), 'failed');
assert.equal(normalizeProviderStatus({ success: true, outcome: 'no_op' }), 'no_op');
assert.equal(providerFailureClassification('task', 'failed'), 'execution_failed');
assert.equal(providerFailureClassification('incomplete', 'failed'), 'execution_failed');
assert.equal(providerFailureClassification('max_turns', 'timeout'), 'timeout');
assert.equal(providerFailureClassification('custom-runtime-detail', 'failed'), 'unknown');
assert.equal(agentTaskFailureCategory({ provider_error: true }, [{ class: 'provider.rate_limit', message: 'rate limit' }], 'provider', 'provider_error'), 'provider.rate_limit');
assert.equal(agentTaskFailureRetryable('provider.rate_limit', 'provider', 'provider_error'), true);
assert.throws(() => normalizeProviderTaskOutcome({}, {}), /request.task_id/);

console.log('✓ agent task outcome normalizer boundary test PASSED');
