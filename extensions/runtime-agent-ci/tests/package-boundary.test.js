'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const packageJson = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'package.json'), 'utf8'));

assert.equal(packageJson.bin['homeboy-run-agent-loop'], undefined, 'run-agent-loop wrapper is retired; use the headless loop entrypoint');
assert.equal(packageJson.bin['homeboy-artifact-fanout'], undefined, 'artifact fanout wrapper is retired; Homeboy owns artifact fanout orchestration');
assert.equal(packageJson.bin['homeboy-run-agent-task-to-review'], undefined, 'agent-task-to-review publication wrapper is retired; Homeboy owns review publication orchestration');
assert.equal(packageJson.exports['./agent-task-to-review-runner'], undefined, 'agent-task-to-review runner is not exported from runtime-agent-ci');

for (const retiredPath of [
  'scripts/run-agent-loop.cjs',
  'scripts/homeboy-artifact-fanout.cjs',
  'scripts/run-agent-task-to-review.cjs',
  'lib/artifact-fanout-materializer.js',
  'lib/agent-task-to-review-runner.js',
]) {
  assert.equal(fs.existsSync(path.join(__dirname, '..', retiredPath)), false, `${retiredPath} should remain deleted`);
}

console.log('runtime-agent-ci package boundary test passed');
