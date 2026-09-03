#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const {
  buildGenericAgentLoopRequest,
  runDeterministicLoop,
  runGenericAgentLoop,
  validateGenericAgentLoopOutcomeContract,
} = require(path.join(repoRoot, 'runtime-agent-ci/generic-orchestration'));

const runtimeProfile = {
  schema: 'homeboy/runtime-profile/v1',
  id: 'example-loop',
  runtime_task_ability: 'example/run-task',
};
const plan = {
  workload_id: 'loop-1',
  workload_label: 'Generic loop fixture',
  target_repo: 'Example/project',
  component_path: '/workspace/project',
  runtime_id: 'example-runtime',
  runtime_profile: runtimeProfile.id,
  runtime_profiles: { [runtimeProfile.id]: runtimeProfile },
  provider: 'fake-provider',
  model: 'fake-model',
  prompt: 'Materialize the fixture.',
  runtime_task: { ability: 'example/run-task', input: { prompt: 'Cook.' } },
  artifact_declarations: [{ name: 'fixture-result', required: true }],
  required_evidence_refs: [{ kind: 'pull_request' }],
  required_runtime_capabilities: ['structured_outcome'],
  time_budget_ms: 120000,
  success_requires_pr: false,
};
const runtime = {
  id: 'example-runtime',
  capabilities: ['structured_outcome'],
  executor: { backend: 'fake-provider', path: '/not-called' },
  manifest: {},
};

const request = buildGenericAgentLoopRequest({ plan, runtime, configPath: '/tmp/agent-loop-plan.json' });
assert.equal(request.schema, 'homeboy/agent-task-request/v1');
assert.equal(request.task_id, 'loop-1');
assert.equal(request.executor.backend, 'fake-provider');
assert.equal(request.executor.config.runtime_profile, 'example-loop');
assert.deepEqual(request.executor.config.runtime_task, { ability: 'example/run-task', input: { prompt: 'Cook.' } });
assert.deepEqual(request.expected_artifacts, ['fixture-result']);
assert.equal(request.limits.timeout_ms, 120000);

let providerSawRequest = false;
const loop = runGenericAgentLoop({
  plan,
  runtime,
  configPath: '/tmp/agent-loop-plan.json',
  validationPolicy: { scenario_id: 'loop-1', success_requires_pr: false },
  execute: ({ request: providerRequest }) => {
    providerSawRequest = true;
    assert.equal(providerRequest.task_id, 'loop-1');
    assert.equal(providerRequest.executor.config.provider, 'fake-provider');
    return {
      schema: 'homeboy/agent-task-outcome/v1',
      task_id: providerRequest.task_id,
      status: 'succeeded',
      summary: 'Fixture provider completed.',
      artifacts: [{ id: 'fixture-result', kind: 'typed-json', artifact_schema: 'example/fixture-result/v1' }],
      evidence_refs: [{ kind: 'pull_request', uri: 'https://github.com/example/project/pull/123', label: 'PR' }],
      metadata: {
        agent_loop_results: {
          scenarios: [{
            id: 'loop-1',
            metadata: {
              job_status: 'completed',
              success_status: 'no_changes',
            },
          }],
        },
      },
    };
  },
});
assert.equal(providerSawRequest, true);
assert.equal(loop.outcome.status, 'succeeded');
assert.equal(loop.assertion.success_status, 'no_changes');
assert.equal(loop.assertion.no_changes_allowed, true);

const adapterLoop = runGenericAgentLoop({
  plan: { ...plan, workload_id: 'adapter-loop', success_requires_pr: true },
  runtime: {
    id: 'fixture-runtime',
    capabilities: ['structured_outcome'],
    executor: { backend: 'fixture', path: '/not-called' },
    manifest: {
      agent_loop: {
        outcome_adapter: {
          module: 'tests/fixtures/generic-agent-outcome-adapter.cjs',
          export: 'scenarioResultsFromOutcome',
        },
      },
    },
  },
  repoRoot,
  validationPolicy: { scenario_id: 'adapter-loop', success_requires_pr: true },
  execute: () => ({
    schema: 'homeboy/agent-task-outcome/v1',
    task_id: 'adapter-loop',
    status: 'succeeded',
    artifacts: [{ id: 'fixture-result', kind: 'typed-json', artifact_schema: 'example/fixture-result/v1' }],
    evidence_refs: [{ kind: 'pull_request', uri: 'https://github.com/example/project/pull/123', label: 'PR' }],
    metadata: {
      fixture: {
        scenarios: [{
          id: 'adapter-loop',
          metadata: {
            job_status: 'completed',
            success_status: 'pr_opened',
          },
        }],
      },
    },
  }),
});
assert.equal(adapterLoop.assertion.success_status, 'pr_opened');

let retryHookCalls = 0;
const deterministicLoop = runDeterministicLoop({
  loopId: 'cook-loop',
  maxIterations: 5,
  state: { value: 0 },
  buildIteration: ({ state }) => ({ next: state.value + 1 }),
  maxAttempts: 2,
  execute: ({ input, attempt }) => {
    if (input.next === 1 && attempt === 1) {
      return { status: 'failed', summary: 'retry once' };
    }
    return {
      status: 'succeeded',
      value: input.next,
      artifacts: [{ name: `packet-${input.next}`, path: `/tmp/packet-${input.next}.json` }],
    };
  },
  reconcile: ({ state, outcome }) => ({ ...state, value: outcome.value, complete: outcome.value === 2 }),
  shouldRetry: ({ outcome }) => {
    retryHookCalls += 1;
    return outcome.status === 'failed';
  },
  stopCriteria: ({ state }) => ({ stop: state.complete === true, reason: state.complete ? 'value_complete' : '' }),
});

assert.equal(deterministicLoop.schema, 'homeboy/deterministic-loop-result/v1');
assert.equal(deterministicLoop.status, 'completed');
assert.equal(deterministicLoop.iterations.length, 2);
assert.equal(deterministicLoop.state.value, 2);
assert.equal(deterministicLoop.iterations[0].attempt, 2);
assert.equal(deterministicLoop.iterations[1].stop.reason, 'value_complete');
assert.equal(retryHookCalls, 2);
assert.deepEqual(
  deterministicLoop.iterations.map((iteration) => iteration.artifacts[0].name),
  ['packet-1', 'packet-2']
);

const contractRequest = {
  task_id: 'contract-loop',
  expected_artifacts: ['fixture-result'],
  artifact_declarations: [{ name: 'fixture-result', required: true, kind: 'typed-json', artifact_schema: 'example/fixture-result/v1' }],
};
const contractOutcome = {
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: 'contract-loop',
  status: 'succeeded',
  artifacts: [{ id: 'fixture-result', kind: 'typed-json', artifact_schema: 'example/fixture-result/v1' }],
  evidence_refs: [{ kind: 'pull_request', uri: 'https://github.com/example/project/pull/123' }],
};

assert.deepEqual(
  validateGenericAgentLoopOutcomeContract({
    request: contractRequest,
    outcome: contractOutcome,
    plan: { required_evidence_refs: [{ kind: 'pull_request' }] },
  }),
  { artifact_count: 1, evidence_ref_count: 1 }
);
assert.throws(
  () => validateGenericAgentLoopOutcomeContract({
    request: contractRequest,
    outcome: { ...contractOutcome, artifacts: [] },
  }),
  /missing expected artifact fixture-result/
);
assert.throws(
  () => validateGenericAgentLoopOutcomeContract({
    request: contractRequest,
    outcome: { ...contractOutcome, artifacts: [{ id: 'fixture-result', kind: 'text', artifact_schema: 'example\/fixture-result\/v1' }] },
  }),
  /expected kind typed-json, got text/
);
assert.throws(
  () => validateGenericAgentLoopOutcomeContract({
    request: contractRequest,
    outcome: { ...contractOutcome, artifacts: [{ id: 'fixture-result', kind: 'typed-json', artifact_schema: 'example\/other\/v1' }] },
  }),
  /expected schema example\/fixture-result\/v1, got example\/other\/v1/
);
assert.throws(
  () => validateGenericAgentLoopOutcomeContract({
    request: contractRequest,
    outcome: { ...contractOutcome, evidence_refs: [{ kind: 'pull_request', uri: 'http://localhost:8888/pr/123' }] },
    plan: { required_evidence_refs: [{ kind: 'pull_request' }] },
  }),
  /local-only/
);

console.log('generic agent loop runner smoke passed');
