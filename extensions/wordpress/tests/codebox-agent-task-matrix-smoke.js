'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

function writeFixtureTaskRunner(root) {
  const fixture = path.join(root, 'fixture-task-runner.cjs');
  const capture = path.join(root, 'capture.jsonl');
  fs.writeFileSync(fixture, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const request = JSON.parse(fs.readFileSync(0, 'utf8'));
fs.appendFileSync(${JSON.stringify(capture)}, JSON.stringify(request) + '\\n');
process.stdout.write(JSON.stringify({
  success: true,
  summary: 'Matrix cell completed: ' + request.orchestrator.agent_task_id,
  artifacts: [{
    id: 'artifact-' + request.orchestrator.agent_task_id,
    kind: 'matrix-cell',
    path: '/artifacts/' + request.orchestrator.agent_task_id + '.json',
    metadata: { matrix: request.context.matrix }
  }],
  evidence_refs: [{
    kind: 'matrix-cell',
    uri: 'homeboy://matrix/' + request.orchestrator.agent_task_id,
    label: 'Matrix cell evidence'
  }],
  metadata: { matrix: request.context.matrix }
}));
`);
  fs.chmodSync(fixture, 0o755);
  return { fixture, capture };
}

function matrixRequests() {
  const axes = {
    model: ['gpt-5.5', 'claude'],
    prompt: ['site-a', 'site-b'],
  };
  const requests = [];
  for (const model of axes.model) {
    for (const prompt of axes.prompt) {
      const matrix = { model, prompt };
      requests.push({
        schema: 'homeboy/agent-task-request/v1',
        task_id: `codebox-matrix[model=${model},prompt=${prompt}]`,
        group_key: 'codebox-matrix-smoke',
        parent_plan_id: 'codebox-matrix-smoke',
        executor: {
          backend: 'codebox',
          model,
          config: { provider: 'openai', max_turns: 1 },
        },
        instructions: `Run smoke task for ${model}/${prompt}.`,
        inputs: { title: 'Codebox matrix smoke', matrix },
        source_refs: [{ kind: 'issue', uri: 'https://github.com/Extra-Chill/homeboy/issues/3210' }],
        workspace: { mode: 'ephemeral' },
        policy: { read: 'sandbox', write: 'sandbox', apply: 'review' },
        limits: { timeout_ms: 30000 },
        expected_artifacts: ['matrix-cell'],
        metadata: { matrix },
      });
    }
  }
  return requests;
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-agent-task-matrix-'));
try {
  const { fixture, capture } = writeFixtureTaskRunner(root);
  const script = path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs');
  const outcomes = matrixRequests().map((request) => {
    const result = spawnSync(process.execPath, [script, '--task-runner', fixture], {
      encoding: 'utf8',
      input: JSON.stringify(request),
    });
    assert.equal(result.status, 0, result.stderr || result.stdout);
    return JSON.parse(result.stdout);
  });

  assert.equal(outcomes.length, 4);
  assert(outcomes.every((outcome) => outcome.schema === 'homeboy/agent-task-outcome/v1'));
  assert(outcomes.every((outcome) => outcome.status === 'succeeded'));
  assert.deepEqual(
    outcomes.map((outcome) => outcome.metadata.codebox.matrix),
    [
      { model: 'gpt-5.5', prompt: 'site-a' },
      { model: 'gpt-5.5', prompt: 'site-b' },
      { model: 'claude', prompt: 'site-a' },
      { model: 'claude', prompt: 'site-b' },
    ]
  );
  assert.equal(outcomes[0].artifacts[0].kind, 'matrix-cell');
  assert.equal(outcomes[3].evidence_refs[0].uri, 'homeboy://matrix/codebox-matrix[model=claude,prompt=site-b]');

  const captured = fs.readFileSync(capture, 'utf8').trim().split('\n').map((line) => JSON.parse(line));
  assert.equal(captured.length, 4);
  assert.equal(captured[0].schema, 'wp-codebox/task-input/v1');
  assert.deepEqual(captured[2].context.matrix, { model: 'claude', prompt: 'site-a' });
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}

console.log('Codebox agent task matrix smoke passed');
