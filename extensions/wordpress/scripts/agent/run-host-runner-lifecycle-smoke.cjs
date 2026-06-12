#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { prepareRunnerCommand, runShellCommand } = require('./run-host-runner-lifecycle.cjs');

assert.equal(prepareRunnerCommand('pnpm verify'), 'corepack pnpm verify');
assert.equal(prepareRunnerCommand(' pnpm install --frozen-lockfile '), 'corepack pnpm install --frozen-lockfile');
assert.equal(prepareRunnerCommand('npm test'), 'npm test');

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-host-runner-lifecycle-'));
try {
  const check = runShellCommand(
    { command: 'pnpm verify', description: 'Verify generated docs' },
    workspace,
    'verification_commands'
  );
  assert.equal(check.command, 'pnpm verify');
  assert.equal(check.prepared_command, 'corepack pnpm verify');
  assert.equal(check.executed_command, 'bash -lc "corepack pnpm verify"');
  assert.equal(typeof check.exit_code, 'number');
  assert.equal(check.workspace, workspace);
} finally {
  fs.rmSync(workspace, { recursive: true, force: true });
}

console.log('Host runner lifecycle smoke passed.');
