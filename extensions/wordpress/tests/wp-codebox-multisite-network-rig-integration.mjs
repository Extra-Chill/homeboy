import { strict as assert } from 'node:assert';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const root = await mkdtemp(path.join(os.tmpdir(), 'wordpress-multisite-rig-integration-'));
const resultFile = path.join(root, 'result.json');
const artifacts = path.join(root, 'artifacts');
const runner = path.resolve(import.meta.dirname, '../rigs/wordpress-multisite-e2e/run.mjs');

try {
  const result = spawnSync(process.execPath, [runner], {
    cwd: root,
    env: {
      ...process.env,
      HOMEBOY_ARTIFACT_ROOT: artifacts,
      HOMEBOY_NETWORK_E2E_RESULT_FILE: resultFile,
    },
    encoding: 'utf8',
  });
  assert.equal(result.status, 0, result.stderr);
  const envelope = JSON.parse(await readFile(resultFile, 'utf8'));
  assert.equal(envelope.success, true);
  assert.deepEqual(
    envelope.result.commands.map((command) => [command.command, command.status]),
    [
      ['wordpress.run-php', 'succeeded'],
      ['wordpress.run-php', 'succeeded'],
      ['wordpress.run-php', 'succeeded'],
      ['wordpress.browser-probe', 'succeeded'],
      ['wordpress.browser-actions', 'succeeded'],
    ],
  );

  const pointer = JSON.parse(await readFile(path.join(artifacts, 'latest-runtime.json'), 'utf8'));
  const browser = path.join(artifacts, pointer.runtimeId, 'files/browser');
  const anonymous = JSON.parse(await readFile(path.join(browser, 'summary.json'), 'utf8'));
  const authenticated = JSON.parse(await readFile(path.join(browser, 'action-summary.json'), 'utf8'));
  assert.equal(anonymous.assertions.failed, 0);
  assert.equal(anonymous.assertions.results.find((assertion) => assertion.id === 'no-console-errors').passed, true);
  assert.equal(authenticated.summary.auth.mode, 'wordpress-admin');
  assert.equal(authenticated.summary.assertions.failed, 0);
  assert.deepEqual(
    authenticated.steps.filter((step) => step.kind === 'navigate').map((step) => new URL(step.finalUrl).pathname),
    ['/alpha/fixture-check/', '/beta/fixture-check/'],
  );
  await readFile(path.join(browser, 'console.jsonl'));
  await readFile(path.join(browser, 'errors.jsonl'));
  assert.ok((await readFile(path.join(browser, 'screenshot.png'))).byteLength > 0);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox multisite network rig integration passed.');
