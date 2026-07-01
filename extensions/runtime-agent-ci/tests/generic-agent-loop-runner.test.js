'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const genericLoopRunner = require('../lib/generic-agent-loop-runner');
const {
  ARTIFACT_MANIFEST_FILE,
  ARTIFACT_MANIFEST_SCHEMA,
} = require('../lib/runtime-contracts.cjs');

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
assert.equal(genericLoop.evidence_envelope.loop_run.iterations[0].accepted, false);
assert.equal(genericLoop.evidence_envelope.loop_run.iterations[1].accepted, true);
assert.equal(genericLoop.evidence.length, 2);

const stoppedUnacceptedLoop = genericLoopRunner.runGenericDeterministicLoop({
  loopId: 'generic-max-iterations-loop',
  maxIterations: 1,
  state: { accepted: false },
  buildTask: ({ iteration }) => ({ iteration }),
  executeTask: () => ({ status: 'succeeded' }),
  collectResult: ({ outcome }) => outcome,
  reconcile: ({ state }) => ({ ...state, accepted: false }),
});
assert.equal(stoppedUnacceptedLoop.iterations[0].stop.reason, 'max_revolutions_reached');
assert.equal(stoppedUnacceptedLoop.evidence_envelope.loop_run.iterations[0].accepted, false);

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
assert.equal(result.productionProof, null);
assert.equal(result.controllerProofValidation, null);

const invalidCandidateResult = genericLoopRunner.runGenericAgentLoop({
  runtime,
  plan: {
    ...plan,
    workload_id: 'invalid-candidate-contract',
    artifact_declarations: [{ name: 'required-packet', kind: 'fixture/Packet/v1', required: true }],
  },
  validate: false,
  validationPolicy: { success_completion_outcomes: ['done'] },
  execute: ({ request }) => ({
    schema: 'homeboy/agent-task-outcome/v1',
    task_id: request.task_id,
    status: 'provider_error',
    summary: 'Provider returned no artifact packet.',
    diagnostics: [{ class: 'fixture.provider_error', message: 'provider failure detail' }],
  }),
});
assert.equal(invalidCandidateResult.outcome.status, 'failed');
assert.match(invalidCandidateResult.outcome.summary, /missing declared artifact required-packet/);
assert.equal(invalidCandidateResult.outcome.diagnostics[0].class, 'fixture.provider_error');
assert.equal(
  invalidCandidateResult.outcome.metadata.generic_agent_loop_contract_validation.invalid_candidate_outcome.status,
  'provider_error'
);
assert.equal(
  invalidCandidateResult.outcome.metadata.generic_agent_loop_contract_validation.invalid_candidate_outcome.summary,
  'Provider returned no artifact packet.'
);

const proofPolicy = { preview_required: true, publication_required: true };
const validProofRun = genericLoopRunner.runGenericAgentLoop({
  runtime,
  plan: { ...plan, workload_id: 'valid-proof-workload' },
  controllerProof: true,
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
  controllerProof: true,
  validate: true,
  validationPolicy: { success_completion_outcomes: ['done'], controller_loop_proof: proofPolicy },
  execute: ({ request }) => proofOutcome(request, [{ kind: 'preview', url: 'https://example.test/preview/missing-publication' }]),
}), /PR or publication evidence is required/);

assert.throws(() => genericLoopRunner.runGenericAgentLoop({
  runtime,
  plan: { ...plan, workload_id: 'missing-preview-evidence' },
  controllerProof: true,
  validate: true,
  validationPolicy: { success_completion_outcomes: ['done'], controller_loop_proof: proofPolicy },
  execute: ({ request }) => proofOutcome(request, [{ kind: 'publication', url: 'https://example.test/pull/125' }]),
}), /Preview materialization evidence is required/);

assert.throws(() => genericLoopRunner.runGenericAgentLoop({
  runtime,
  plan: { ...plan, workload_id: 'local-proof-evidence' },
  controllerProof: true,
  validate: true,
  validationPolicy: { success_completion_outcomes: ['done'], controller_loop_proof: proofPolicy },
  execute: ({ request }) => proofOutcome(request, [
    { kind: 'preview', url: 'http://localhost:8888/preview' },
    { kind: 'publication', url: 'https://example.test/pull/124' },
  ]),
}), /Reviewer-facing evidence must use a durable non-local ref: preview/);

const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-generic-agent-loop-'));
try {
  const requestCapturePath = path.join(tmpRoot, 'request.json');
  const shellExecutorPath = path.join(tmpRoot, 'fake-non-node-executor');
  fs.writeFileSync(shellExecutorPath, `#!/bin/sh
cat > "$HOMEBOY_REQUEST_CAPTURE"
printf '%s\n' '{"schema":"homeboy/agent-task-outcome/v1","task_id":"manifest-shell","status":"succeeded","summary":"Shell executor completed.","metadata":{"results":{"scenarios":[{"id":"manifest-shell","metrics":{"generic_agent_task_executor_mean":1},"metadata":{"job_status":"completed","success_status":"no_changes","completion_outcome":"done","completion_outcome_satisfied":true,"executor_env":"'"$HOMEBOY_EXECUTOR_ENV"'","allowed_secret":"'"$ALLOWED_SECRET"'","leaked_env":"'"$UNDECLARED_SECRET"'"}}]}}}'
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
    plan: { ...plan, workload_id: 'manifest-shell', secret_env: ['ALLOWED_SECRET'] },
    validationPolicy: { success_completion_outcomes: ['done'] },
    env: { ...process.env, ALLOWED_SECRET: 'declared-secret', UNDECLARED_SECRET: 'ambient-secret' },
  });
  assert.equal(shellRun.outcome.status, 'succeeded');
  assert.equal(shellRun.outcome.metadata.runtime_invocation_result.command, shellExecutorPath);
  assert.equal(shellRun.results.scenarios[0].metadata.executor_env, 'from-manifest');
  assert.equal(shellRun.results.scenarios[0].metadata.allowed_secret, 'declared-secret');
  assert.equal(shellRun.results.scenarios[0].metadata.leaked_env, '');
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

  const noisyExecutorPath = path.join(tmpRoot, 'fake-noisy-executor.cjs');
  const noisyArtifactsDir = path.join(tmpRoot, 'artifacts');
  const lateSentinel = 'LATE_PROVIDER_STDERR_SENTINEL';
  fs.writeFileSync(noisyExecutorPath, `'use strict';
const request = JSON.parse(require('node:fs').readFileSync(0, 'utf8'));
process.stderr.write('provider stderr prefix\\n' + 'x'.repeat(20000) + '${lateSentinel}\\n');
process.stdout.write(JSON.stringify({
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: request.task_id,
  status: 'succeeded',
  summary: 'Noisy executor completed.',
  metadata: {
    results: { scenarios: [{ id: request.task_id, metrics: { generic_agent_task_executor_mean: 1 }, metadata: { job_status: 'completed', success_status: 'no_changes', completion_outcome: 'done', completion_outcome_satisfied: true } }] },
  },
}));
`);
  let capturedStderr = '';
  const originalStderrWrite = process.stderr.write;
  process.stderr.write = (chunk, encoding, callback) => {
    capturedStderr += String(chunk);
    if (typeof callback === 'function') {
      callback();
    }
    return true;
  };
  let noisyRun;
  try {
    noisyRun = genericLoopRunner.runGenericAgentLoop({
      runtime: {
        id: 'manifest-noisy-runtime',
        executor: {
          backend: 'node-fixture',
          invocation: {
            command: process.execPath,
            argv: [noisyExecutorPath],
            cwd: tmpRoot,
            stdin: 'request_json',
            stdout: 'outcome_json',
            stderr: 'inherit',
          },
        },
      },
      plan: { ...plan, workload_id: 'manifest-noisy', artifacts_path: noisyArtifactsDir },
      validationPolicy: { success_completion_outcomes: ['done'] },
      stderrMaxBytes: 256,
    });
  } finally {
    process.stderr.write = originalStderrWrite;
  }
  assert.equal(noisyRun.outcome.status, 'succeeded');
  assert.match(capturedStderr, /Runtime stderr artifact:/);
  assert.doesNotMatch(capturedStderr, new RegExp(lateSentinel), 'large provider stderr must not be inherited wholesale');
  const stderrArtifact = noisyRun.outcome.metadata.runtime_invocation_result.stderr_artifact;
  assert.equal(stderrArtifact.kind, 'runtime-stderr');
  assert.equal(fs.readFileSync(stderrArtifact.path, 'utf8').includes(lateSentinel), true, 'full provider stderr is preserved as an artifact');

  const sharedArtifactDir = path.join(tmpRoot, 'shared-artifacts');
  const sharedArtifactsRun = genericLoopRunner.runGenericAgentLoop({
    runtime: {
      id: 'manifest-shared-artifact-runtime',
      executor: {
        backend: 'node-fixture',
        invocation: {
          command: process.execPath,
          argv: [noisyExecutorPath],
          cwd: tmpRoot,
          stdin: 'request_json',
          stdout: 'outcome_json',
        },
      },
    },
    plan: { ...plan, workload_id: 'shared-artifact-paths' },
    validationPolicy: { success_completion_outcomes: ['done'] },
    artifact_paths: { run_dir: sharedArtifactDir },
  });
  assert.equal(sharedArtifactsRun.outcome.metadata.runtime_invocation_result.stderr_artifact.path, path.join(sharedArtifactDir, 'shared-artifact-paths-runtime-stderr.txt'));
  genericLoopRunner.writeGenericAgentLoopArtifacts({
    outcome: sharedArtifactsRun.outcome,
    results: sharedArtifactsRun.results,
    artifact_paths: { run_dir: sharedArtifactDir },
  });
  const sharedManifest = JSON.parse(fs.readFileSync(path.join(sharedArtifactDir, ARTIFACT_MANIFEST_FILE), 'utf8'));
  assert.equal(sharedManifest.schema, ARTIFACT_MANIFEST_SCHEMA);
  assert.deepEqual(sharedManifest.artifacts.map((artifact) => artifact.path).sort(), [
    'outcome.json',
    'results.json',
    'shared-artifact-paths-runtime-stderr.txt',
    'status.json',
  ]);
  assert.equal(sharedManifest.artifacts.every((artifact) => !path.isAbsolute(artifact.path)), true);

  let capturedEnv = null;
  genericLoopRunner.runGenericAgentLoop({
    runtime: {
      id: 'env-allowlist-runtime',
      executor: {
        backend: 'fixture',
        invocation: {
          command: 'node',
          argv: ['fixture-runtime.js'],
          env_allowlist: ['HOMEBOY_WP_CODEBOX_CORE_MODULE'],
        },
      },
    },
    request: {
      schema: 'homeboy/agent-task-request/v1',
      task_id: 'env-allowlist-runtime',
      instructions: 'Exercise runtime env allowlist merge.',
      executor: {
        backend: 'fixture',
        config: { env_allowlist: [] },
      },
    },
    plan: { ...plan, workload_id: 'env-allowlist-runtime' },
    validationPolicy: { success_completion_outcomes: ['done'] },
    env: { HOMEBOY_WP_CODEBOX_CORE_MODULE: '/tmp/wp-codebox-core/contracts.js' },
    spawnSync: (command, argv, options) => {
      capturedEnv = options.env;
      return {
        status: 0,
        stdout: JSON.stringify({
          schema: 'homeboy/agent-task-outcome/v1',
          task_id: 'env-allowlist-runtime',
          status: 'succeeded',
          metadata: { results: { scenarios: [{ id: 'env-allowlist-runtime', metadata: { completion_outcome: 'done', completion_outcome_satisfied: true } }] } },
        }),
        stderr: '',
      };
    },
  });
  assert.equal(capturedEnv.HOMEBOY_WP_CODEBOX_CORE_MODULE, '/tmp/wp-codebox-core/contracts.js');
  const inheritEnvRun = genericLoopRunner.runGenericAgentLoop({
    runtime: {
      id: 'env-inherit-runtime',
      executor: { backend: 'fixture', invocation: { command: 'node', inherit_env: true } },
    },
    request: {
      schema: 'homeboy/agent-task-request/v1',
      task_id: 'env-inherit-runtime',
      instructions: 'Exercise runtime inherit_env rejection.',
      executor: { backend: 'fixture', config: {} },
    },
    plan: { ...plan, workload_id: 'env-inherit-runtime' },
    validationPolicy: { success_completion_outcomes: ['done'] },
    validate: false,
    spawnSync: () => ({ status: 0, stdout: '{}', stderr: '' }),
  });
  assert.equal(inheritEnvRun.outcome.status, 'failed');
  assert.match(inheritEnvRun.outcome.summary, /ambient env inheritance is not supported/);

  const hugePayloadSentinel = 'WHOLESALE_RESULT_PAYLOAD_SENTINEL';
  const stdoutSummary = genericLoopRunner.genericAgentLoopStdoutSummary({
    outcome: {
      schema: 'homeboy/agent-task-outcome/v1',
      task_id: 'large-payload',
      status: 'succeeded',
      summary: 'Large payload outcome completed.',
      metadata: {
        provider_result: `${'y'.repeat(20000)}${hugePayloadSentinel}`,
      },
      artifacts: [{ name: 'provider-result', kind: 'json', path: '/tmp/provider-result.json', payload: `${'z'.repeat(20000)}${hugePayloadSentinel}` }],
      evidence_refs: [{ kind: 'provider-result', uri: 'artifact://provider-result' }],
    },
    results: { scenarios: [{ id: 'large-payload' }] },
    outcomeFile: '/tmp/outcome.json',
    resultsFile: '/tmp/results.json',
  });
  const stdoutSummaryJson = JSON.stringify(stdoutSummary);
  assert.doesNotMatch(stdoutSummaryJson, new RegExp(hugePayloadSentinel), 'stdout summary must not include full provider result or artifact payloads');
  assert.equal(stdoutSummary.files.outcome, '/tmp/outcome.json');
  assert.equal(stdoutSummary.artifact_refs[0].path, '/tmp/provider-result.json');
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

// A hard runtime failure routes the underlying agent/CLI stderr into the outcome
// diagnostics (not its own stderr). Surface that detail to the job's stderr and
// persist it as the runtime stderr artifact so the real crash reason is visible
// even when a later assertion throws a generic message and results.json is never
// written.
(() => {
  const runDir = fs.mkdtempSync(path.join(os.tmpdir(), 'runtime-agent-stderr-surface-'));
  const executorScript = path.join(runDir, 'failing-executor.cjs');
  fs.writeFileSync(executorScript, `'use strict';
const outcome = {
  schema: 'homeboy/agent-task-outcome/v1',
  task_id: 'technical-docs-bootstrap-flow',
  status: 'failed',
  summary: 'WP Codebox agent-task-run failed.',
  diagnostics: [{
    class: 'wp-codebox.agent_task_run_failed',
    message: 'WP Codebox agent-task-run failed.',
    data: { status: 1, stderr: 'PLAYGROUND_BOOT_FATAL: sandbox could not start' },
  }],
};
process.stdout.write(JSON.stringify(outcome));
process.exitCode = 1;
`);

  const runtime = {
    id: 'wp-codebox',
    executor: {
      backend: 'wp-codebox',
      invocation: { command: process.execPath, argv: [executorScript], stderr: 'inherit_on_failure' },
    },
  };
  const request = {
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'technical-docs-bootstrap-flow',
    instructions: 'run',
    executor: { backend: 'wp-codebox' },
  };

  const captured = [];
  const originalWrite = process.stderr.write;
  process.stderr.write = (chunk, ...rest) => {
    captured.push(String(chunk));
    return originalWrite.call(process.stderr, chunk, ...rest);
  };
  let result;
  try {
    result = genericLoopRunner.runGenericAgentLoop({
      runtime,
      request,
      validate: false,
      run_dir: runDir,
    });
  } finally {
    process.stderr.write = originalWrite;
  }

  const stderrText = captured.join('');
  assert.ok(
    stderrText.includes('PLAYGROUND_BOOT_FATAL: sandbox could not start'),
    'failed runtime outcome diagnostics must be surfaced to stderr'
  );

  const stderrFile = path.join(runDir, 'technical-docs-bootstrap-flow-runtime-stderr.txt');
  assert.ok(fs.existsSync(stderrFile), 'runtime stderr artifact file must be written on failure');
  assert.ok(
    fs.readFileSync(stderrFile, 'utf8').includes('PLAYGROUND_BOOT_FATAL: sandbox could not start'),
    'runtime stderr artifact must contain the underlying failure detail'
  );
  assert.equal(result.outcome.status, 'failed');

  fs.rmSync(runDir, { recursive: true, force: true });
})();

process.stdout.write('Generic agent loop runner behavior checks passed\n');
