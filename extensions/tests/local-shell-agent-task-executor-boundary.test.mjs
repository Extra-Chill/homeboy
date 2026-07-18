#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const executor = path.join(
  repoRoot,
  'agent-runtimes',
  'local-shell',
  'scripts',
  'agent',
  'homeboy-local-shell-agent-task-executor.cjs'
);
const request = {
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'local-shell-evidence-uri-boundary',
  executor: { backend: 'local-shell' },
  instructions: 'Accept this representative local-shell task request.',
};

const result = spawnSync(process.execPath, [executor], {
  encoding: 'utf8',
  input: JSON.stringify(request),
});

assert.equal(result.status, 0, result.stderr || result.stdout);
const outcome = JSON.parse(result.stdout);
assert.equal(outcome.schema, 'homeboy/agent-task-outcome/v1');
assert.equal(outcome.task_id, request.task_id);
assert.equal(outcome.status, 'no_op');
assert.deepEqual(outcome.evidence_refs, [{
  kind: 'preview',
  uri: 'https://example.test/local-shell-evidence-uri-boundary/preview',
}]);
assert.equal(outcome.metadata.backend, 'local-shell');

console.log('local-shell agent-task executor boundary passed');
