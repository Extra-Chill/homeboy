'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const fixtureCodeboxCoreModule = path.join(__dirname, 'fixtures', 'wp-codebox-core-agent-task-normalizer.mjs');
const executor = path.join(
  __dirname,
  '..',
  '..',
  'agent-runtimes',
  'wp-codebox',
  'scripts',
  'agent',
  'homeboy-codebox-agent-task-executor.cjs'
);

function fixtureEnv(overrides = {}) {
  return {
    ...process.env,
    HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCodeboxCoreModule,
    ...overrides,
  };
}

function writeTaskRunner(root, body) {
  const file = path.join(root, `task-runner-${Math.random().toString(16).slice(2)}.cjs`);
  fs.writeFileSync(file, `#!/usr/bin/env node\n'use strict';\n${body}\n`);
  fs.chmodSync(file, 0o755);
  return file;
}

function runExecutor(request, taskRunner, env = {}) {
  return spawnSync(process.execPath, [executor, '--task-runner', taskRunner], {
    encoding: 'utf8',
    env: fixtureEnv(env),
    input: JSON.stringify(request),
  });
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-agent-executor-smoke-'));
try {
  const capturePath = path.join(root, 'capture.json');
  const request = {
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'executor-smoke-task',
    executor: {
      backend: 'wp-codebox',
      model: 'gpt-5.5',
      config: {
        provider: 'openai',
        provider_plugin_paths: ['/providers/openai'],
        secret_env: ['OPENAI_API_KEY'],
      },
    },
    instructions: 'Capture review evidence.',
    inputs: { finding: 'missing evidence' },
    workspace: { mode: 'ephemeral' },
    expected_artifacts: ['screenshot'],
  };

  const successRunner = writeTaskRunner(root, `
const fs = require('node:fs');
const request = JSON.parse(fs.readFileSync(0, 'utf8'));
fs.writeFileSync(${JSON.stringify(capturePath)}, JSON.stringify(request, null, 2));
process.stdout.write(JSON.stringify({
  success: true,
  status: 'completed',
  summary: 'Sandbox completed.',
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    metadata: { changed_files_count: 1, patch_sha256: 'fixture-patch-sha' },
    artifact_refs: [{ id: 'artifact-1', kind: 'screenshot', path: '/artifacts/screenshot.png' }],
    evidence_refs: [{ kind: 'preview', uri: 'https://example.test/preview', label: 'Preview' }],
    result: { outputs: { review_ready: true } }
  },
  run: { runId: 'fixture-run-1', status: 'succeeded', runtime: { id: 'fixture-runtime-1', status: 'destroyed' } }
}));
`);
  const success = runExecutor(request, successRunner);
  assert.equal(success.status, 0, success.stderr || success.stdout);
  const successOutcome = JSON.parse(success.stdout);
  assert.equal(successOutcome.status, 'succeeded');
  assert.equal(successOutcome.outputs.review_ready, true);
  assert.equal(successOutcome.artifacts.some((artifact) => artifact.kind === 'screenshot'), true);
  assert.equal(successOutcome.evidence_refs.some((ref) => ref.uri === 'https://example.test/preview'), true);

  const captured = JSON.parse(fs.readFileSync(capturePath, 'utf8'));
  assert.equal(captured.schema, 'wp-codebox/task-input/v1');
  assert.equal(captured.orchestrator.agent_task_id, 'executor-smoke-task');
  assert.equal(captured.provider, 'openai');
  assert.deepEqual(captured.secret_env, ['OPENAI_API_KEY']);

  const failureRunner = writeTaskRunner(root, `
process.stdout.write(JSON.stringify({
  success: false,
  status: 'failed',
  summary: 'Provider auth failed.',
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'failed',
    result: { outputs: {} }
  },
  diagnostics: [{ class: 'provider.auth_failed', message: 'Provider auth failed.' }]
}));
process.exit(7);
`);
  const failure = runExecutor({ ...request, task_id: 'executor-provider-failure' }, failureRunner);
  assert.equal(failure.status, 1, failure.stderr || failure.stdout);
  const failureOutcome = JSON.parse(failure.stdout);
  assert.equal(failureOutcome.status, 'failed');
  assert.equal(failureOutcome.failure_classification, 'execution_failed');
  assert.equal(failureOutcome.diagnostics.length > 0, true);
  assert.match(failureOutcome.summary, /Embedded agent runtime failed/);
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
