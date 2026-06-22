'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const genericLoopRunner = require('../lib/generic-agent-loop-runner');

assert.equal(
  Object.prototype.hasOwnProperty.call(genericLoopRunner, 'runDeterministicLoop'),
  false,
  'generic runtime adapter must not export deterministic loop internals'
);

const genericLoop = genericLoopRunner.runGenericDeterministicLoop({
  loopId: 'generic-reconcile-loop',
  maxIterations: 3,
  state: { accepted: false },
  buildTask: ({ iteration, results }) => ({
    iteration,
    previous_value: results[results.length - 1]?.value || 0,
  }),
  executeTask: ({ task }) => ({
    status: 'succeeded',
    value: task.previous_value + 1,
    evidence_refs: [{ kind: 'iteration-output', uri: `https://example.test/evidence/${task.iteration}` }],
  }),
  collectResult: ({ outcome }) => outcome,
  reconcile: ({ state, result }) => ({
    ...state,
    accepted: result.value >= 2,
    latest_value: result.value,
  }),
  stopPolicy: ({ state }) => state.accepted
    ? { stop: true, reason: 'reconcile_criteria_satisfied' }
    : { stop: false },
  shouldContinue: ({ state }) => !state.accepted,
});

assert.equal(genericLoop.schema, 'homeboy/generic-deterministic-loop-output/v1');
assert.equal(genericLoop.deterministic_loop_schema, 'homeboy/deterministic-loop-result/v1');
assert.equal(genericLoop.status, 'completed');
assert.equal(genericLoop.iterations.length, 2);
assert.equal(genericLoop.tasks[0].previous_value, 0);
assert.equal(genericLoop.tasks[1].previous_value, 1);
assert.equal(genericLoop.results[1].value, 2);
assert.equal(genericLoop.state.accepted, true);
assert.equal(genericLoop.iterations[0].stop.stop, false);
assert.equal(genericLoop.iterations[1].stop.reason, 'reconcile_criteria_satisfied');
assert.equal(genericLoop.evidence_envelope.schema, 'homeboy/generic-deterministic-loop-evidence/v1');
assert.equal(genericLoop.evidence_envelope.iteration_count, 2);
assert.equal(genericLoop.evidence.length, 2);

const runtime = { id: 'fixture-runtime', executor: { backend: 'fixture', path: '/unused' } };
const plan = {
  workload_id: 'fixture-workload',
  target_repo: 'Extra-Chill/example',
  component_path: '/workspace/example',
  runtime_profile: 'runtime-agent-ci',
  runtime_profiles: {
    'runtime-agent-ci': {
      id: 'runtime-agent-ci',
      runtime_task_ability: 'fixture/run',
    },
  },
  success_completion_outcomes: ['done'],
};
let executeCalls = 0;

const result = genericLoopRunner.runGenericAgentLoop({
  runtime,
  plan,
  validate: true,
  validationPolicy: { success_completion_outcomes: ['done'] },
  execute: ({ request }) => {
    executeCalls += 1;
    assert.equal(request.schema, 'homeboy/agent-task-request/v1');
    assert.equal(request.task_id, 'fixture-workload');
    return {
      schema: 'homeboy/agent-task-outcome/v1',
      task_id: request.task_id,
      status: 'succeeded',
      summary: 'Fixture executor completed.',
      metadata: {
        results: {
          scenarios: [{
            id: request.task_id,
            metrics: { generic_agent_task_executor_mean: 1 },
            metadata: {
              job_status: 'completed',
              success_status: 'no_changes',
              completion_outcome: 'done',
              completion_outcome_satisfied: true,
            },
          }],
        },
      },
    };
  },
});

assert.equal(executeCalls, 1);
assert.equal(result.request.task_id, 'fixture-workload');
assert.equal(result.outcome.status, 'succeeded');
assert.equal(result.results.scenarios[0].id, 'fixture-workload');
assert.equal(result.assertion.completion_outcome_satisfied, true);
assert.equal(result.loop.schema, 'homeboy/generic-deterministic-loop-output/v1');
assert.equal(result.loop.deterministic_loop_schema, 'homeboy/deterministic-loop-result/v1');
assert.equal(result.loop.status, 'completed');
assert.equal(result.loop.state.status, 'succeeded');
assert.equal(result.loop.iterations.length, 1);
assert.equal(result.loop.results.length, 1);
assert.equal(result.loop.outcome.status, 'succeeded');

const proofPolicy = { preview_required: true, publication_required: true };
const validProofRun = genericLoopRunner.runGenericAgentLoop({
  runtime,
  plan: { ...plan, workload_id: 'valid-proof-workload' },
  validate: true,
  validationPolicy: { success_completion_outcomes: ['done'], controller_loop_proof: proofPolicy },
  execute: ({ request }) => proofOutcome(request, [
    { kind: 'preview', url: 'https://example.test/preview/valid-proof-workload' },
    { kind: 'publication', url: 'https://example.test/pull/123' },
  ]),
});
assert.equal(validProofRun.productionProof.status, 'succeeded');
assert.equal(validProofRun.controllerProofValidation.valid, true);
assert.equal(validProofRun.results.scenarios[0].metadata.controller_loop_proof_validation.valid, true);

assert.throws(() => genericLoopRunner.runGenericAgentLoop({
  runtime,
  plan: { ...plan, workload_id: 'missing-proof-evidence' },
  validate: true,
  validationPolicy: { success_completion_outcomes: ['done'], controller_loop_proof: proofPolicy },
  execute: ({ request }) => proofOutcome(request, [{ kind: 'preview', url: 'https://example.test/preview/missing-publication' }]),
}), /PR or publication evidence is required/);

assert.throws(() => genericLoopRunner.runGenericAgentLoop({
  runtime,
  plan: { ...plan, workload_id: 'missing-preview-evidence' },
  validate: true,
  validationPolicy: { success_completion_outcomes: ['done'], controller_loop_proof: proofPolicy },
  execute: ({ request }) => proofOutcome(request, [{ kind: 'publication', url: 'https://example.test/pull/125' }]),
}), /Preview materialization evidence is required/);

assert.throws(() => genericLoopRunner.runGenericAgentLoop({
  runtime,
  plan: { ...plan, workload_id: 'local-proof-evidence' },
  validate: true,
  validationPolicy: { success_completion_outcomes: ['done'], controller_loop_proof: proofPolicy },
  execute: ({ request }) => proofOutcome(request, [
    { kind: 'preview', url: 'http://localhost:8888/preview' },
    { kind: 'publication', url: 'https://example.test/pull/124' },
  ]),
}), /Reviewer-facing evidence must use a durable non-local URL: preview/);

const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-generic-agent-loop-'));
try {
  const requestCapturePath = path.join(tmpRoot, 'request.json');
  const shellExecutorPath = path.join(tmpRoot, 'fake-non-node-executor');
  fs.writeFileSync(shellExecutorPath, `#!/bin/sh
cat > "$HOMEBOY_REQUEST_CAPTURE"
printf '%s\n' '{"schema":"homeboy/agent-task-outcome/v1","task_id":"manifest-shell","status":"succeeded","summary":"Shell executor completed.","metadata":{"results":{"scenarios":[{"id":"manifest-shell","metrics":{"generic_agent_task_executor_mean":1},"metadata":{"job_status":"completed","success_status":"no_changes","completion_outcome":"done","completion_outcome_satisfied":true,"executor_env":"'"$HOMEBOY_EXECUTOR_ENV"'"}}]}}}'
`);
  fs.chmodSync(shellExecutorPath, 0o755);

  const shellRun = genericLoopRunner.runGenericAgentLoop({
    runtime: {
      id: 'manifest-shell-runtime',
      executor: {
        backend: 'shell-fixture',
        invocation: {
          command: shellExecutorPath,
          argv: [],
          cwd: tmpRoot,
          env: {
            HOMEBOY_REQUEST_CAPTURE: requestCapturePath,
            HOMEBOY_EXECUTOR_ENV: 'from-manifest',
          },
          stdin: 'request_json',
          stdout: 'outcome_json',
        },
      },
    },
    plan: { ...plan, workload_id: 'manifest-shell' },
    validationPolicy: { success_completion_outcomes: ['done'] },
  });
  assert.equal(shellRun.outcome.status, 'succeeded');
  assert.equal(shellRun.outcome.metadata.runtime_invocation_result.command, shellExecutorPath);
  assert.equal(shellRun.results.scenarios[0].metadata.executor_env, 'from-manifest');
  assert.equal(JSON.parse(fs.readFileSync(requestCapturePath, 'utf8')).task_id, 'manifest-shell');

  const nodeExecutorPath = path.join(tmpRoot, 'fake-node-executor.cjs');
  fs.writeFileSync(nodeExecutorPath, `'use strict';
const request = JSON.parse(require('node:fs').readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: request.task_id,
  status: 'succeeded',
  summary: 'Node executor completed.',
  metadata: {
    results: { scenarios: [{ id: request.task_id, metrics: { generic_agent_task_executor_mean: 1 }, metadata: { job_status: 'completed', success_status: 'no_changes', completion_outcome: 'done', completion_outcome_satisfied: true } }] },
  },
}));
`);
  const nodeRun = genericLoopRunner.runGenericAgentLoop({
    runtime: {
      id: 'manifest-node-runtime',
      executor: {
        backend: 'node-fixture',
        invocation: {
          command: process.execPath,
          argv: [nodeExecutorPath],
          cwd: tmpRoot,
          stdin: 'request_json',
          stdout: 'outcome_json',
        },
      },
    },
    plan: { ...plan, workload_id: 'manifest-node' },
    validationPolicy: { success_completion_outcomes: ['done'] },
  });
  assert.equal(nodeRun.outcome.status, 'succeeded');
  assert.equal(nodeRun.outcome.metadata.runtime_invocation_result.command, process.execPath);
  assert.deepEqual(nodeRun.outcome.metadata.runtime_invocation_result.argv, [nodeExecutorPath]);
} finally {
  fs.rmSync(tmpRoot, { recursive: true, force: true });
}

function proofOutcome(request, evidenceRefs) {
  return {
    schema: 'homeboy/agent-task-outcome/v1',
    task_id: request.task_id,
    status: 'succeeded',
    summary: 'Fixture executor completed.',
    evidence_refs: evidenceRefs,
    metadata: {
      results: {
        scenarios: [{
          id: request.task_id,
          metrics: { generic_agent_task_executor_mean: 1 },
          metadata: {
            job_status: 'completed',
            success_status: 'no_changes',
            completion_outcome: 'done',
            completion_outcome_satisfied: true,
          },
        }],
      },
    },
  };
}

process.stdout.write('Generic agent loop runner behavior checks passed\n');
