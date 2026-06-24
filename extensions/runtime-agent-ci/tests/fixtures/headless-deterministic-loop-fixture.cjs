'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

function createHeadlessDeterministicLoopFixture(options = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-headless-loop-fixture-'));
  const runtimeId = options.runtimeId || 'headless-fixture-runtime';
  const runtimeRoot = path.join(root, 'agent-runtimes', runtimeId);
  const artifactDir = path.join(root, 'artifacts');
  fs.mkdirSync(runtimeRoot, { recursive: true });
  fs.mkdirSync(artifactDir, { recursive: true });
  fs.copyFileSync(
    path.join(__dirname, 'fake-agent-task-executor.cjs'),
    path.join(runtimeRoot, 'fake-agent-task-executor.cjs')
  );

  return {
    root,
    artifactDir,
    spec: {
      schema: 'homeboy/headless-deterministic-loop/v1',
      loop_id: 'headless-fixture-loop',
      workload_id: 'headless-fixture-task',
      workload_label: 'Headless deterministic fixture task',
      target_repo: 'Example/project',
      component_path: '/workspace/project',
      runtime_id: runtimeId,
      runtime_profile: 'headless-fixture-profile',
      runtime_profiles: {
        'headless-fixture-profile': {
          id: 'headless-fixture-profile',
          runtime_task_ability: 'fixture/run-task',
        },
      },
      runtime_manifest: {
        schema: 'homeboy/agent-runtime-manifest/v1',
        id: runtimeId,
        agent_task_executors: [{
          id: 'fake-provider',
          backend: 'headless-fixture',
          status: 'active',
          invocation: { argv: ['node', '{{runtime_path}}/fake-agent-task-executor.cjs'] },
        }],
      },
      runtime_task: { ability: 'fixture/run-task', input: { prompt: 'Materialize deterministic fixture evidence.' } },
      artifact_declarations: [{ name: 'headless-fixture-evidence', required: true }],
      success_requires_pr: false,
    },
  };
}

async function runHeadlessDeterministicLoopFixture(options = {}) {
  const fixture = createHeadlessDeterministicLoopFixture(options);
  const repoRoot = path.resolve(__dirname, '..', '..', '..');
  const { runHeadlessDeterministicLoop } = require(path.join(repoRoot, 'runtime-agent-ci'));
  const result = await runHeadlessDeterministicLoop({
    spec: fixture.spec,
    repoRoot: fixture.root,
    env: {
      ...process.env,
      HOMEBOY_HEADLESS_FIXTURE_ARTIFACT_DIR: fixture.artifactDir,
    },
  });
  return { ...fixture, result };
}

function assertHeadlessDeterministicLoopFixture(run) {
  const { result } = run;
  assert.equal(result.schema, 'homeboy/headless-deterministic-loop-result/v1');
  assert.equal(result.status, 'succeeded');
  assert.equal(result.runtime.backend, 'headless-fixture');
  assert.equal(result.tasks.length, 1);
  assert.equal(result.tasks[0].request.schema, 'homeboy/agent-task-request/v1');
  assert.equal(result.tasks[0].outcome.schema, 'homeboy/agent-task-outcome/v1');
  assert.equal(result.tasks[0].loop.schema, 'homeboy/generic-deterministic-loop-output/v1');
  assert.equal(result.tasks[0].loop.deterministic_loop_schema, 'homeboy/deterministic-loop-result/v1');
  assert.equal(result.tasks[0].loop.status, 'completed');
  assert.equal(result.tasks[0].state.status, 'succeeded');
  assert.equal(result.tasks[0].state.artifacts.length, 1);
  assert.equal(result.tasks[0].state.artifacts[0].schema, 'homeboy/deterministic-loop-artifact/v1');
  assert.equal(result.tasks[0].state.artifacts[0].name, 'headless-fixture-evidence');
  assert.equal(result.results.scenarios[0].metadata.evidence_schema, 'homeboy/headless-deterministic-loop-evidence/v1');
  assert.deepEqual(result.events.map((event) => event.type), [
    'loop_started',
    'task_planned',
    'task_started',
    'task_completed',
    'task_asserted',
    'loop_completed',
  ]);

  const evidencePath = result.tasks[0].state.artifacts[0].path;
  const evidence = JSON.parse(fs.readFileSync(evidencePath, 'utf8'));
  assert.equal(evidence.schema, 'homeboy/headless-deterministic-loop-evidence/v1');
  assert.equal(evidence.task_id, 'headless-fixture-task');
  assert.deepEqual(evidence.expected_artifacts, ['headless-fixture-evidence']);
}

module.exports = {
  assertHeadlessDeterministicLoopFixture,
  createHeadlessDeterministicLoopFixture,
  runHeadlessDeterministicLoopFixture,
};
