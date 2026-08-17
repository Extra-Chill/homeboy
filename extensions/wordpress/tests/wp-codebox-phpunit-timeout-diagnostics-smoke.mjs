import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-timeout-'));
const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const component = path.join(root, 'sample-plugin');
const artifacts = path.join(root, 'artifacts');
const invocationArtifacts = path.join(root, 'invocation-artifacts');
const cli = path.join(root, 'wp-codebox');

function pidAlive(pid) {
  try { process.kill(pid, 0); return true; } catch { return false; }
}

async function waitForReaped(pid, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!pidAlive(pid)) { return true; }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  return false;
}

await mkdir(component, { recursive: true });
await mkdir(invocationArtifacts, { recursive: true });
await writeFile(path.join(component, 'sample-plugin.php'), '<?php\n/* Plugin Name: Sample Plugin */\n');
await writeFile(path.join(invocationArtifacts, 'homeboy-artifact-manifest.json'), '{"schema":"homeboy/artifact-manifest/v1"}\n');
await writeFile(cli, `#!/usr/bin/env node
import fs from 'node:fs';
import { spawn } from 'node:child_process';
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args[0] === 'recipe' && args[1] === 'build') {
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
const byteMap = Object.fromEntries(Array.from({ length: 131072 }, (_, index) => [index, index % 256]));
const artifacts = args[args.indexOf('--artifacts') + 1];
const runtime = artifacts + '/runtime-fixture';
fs.mkdirSync(runtime + '/files/phpunit', { recursive: true });
fs.writeFileSync(artifacts + '/latest-runtime.json', JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
fs.writeFileSync(runtime + '/files/phpunit-output.log', 'x'.repeat(2 * 1024 * 1024));
fs.writeFileSync(runtime + '/files/phpunit/.pg-test-result.txt', 'STAGE_FAIL:bootstrap ' + 'y'.repeat(2 * 1024 * 1024));
process.stdout.write(JSON.stringify({ executions: [{ command: 'phpunit Authorization: Bearer fixture-bearer-secret', status: 'running', stdout: byteMap }] }));
process.stderr.write('Cookie: session=fixture-cookie-secret\\nrequest=https://user:fixture-url-secret@example.test/private\\n');
const descendant = spawn(process.execPath, ['-e', 'process.on("SIGTERM", () => {}); setInterval(() => {}, 1e9);'], { stdio: 'inherit' });
fs.writeFileSync(process.env.FIXTURE_PIDS_PATH, JSON.stringify({ descendant: descendant.pid }));
process.on('SIGTERM', () => process.exit(0));
setTimeout(() => {}, 10000);
`);
await chmod(cli, 0o755);

try {
  const startedAt = Date.now();
  const run = spawnSync(runner, [], {
    env: {
      ...process.env,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'sample-plugin',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts,
      HOMEBOY_INVOCATION_ARTIFACT_DIR: invocationArtifacts,
      HOMEBOY_SETTINGS_JSON: '{}',
      HOMEBOY_WORDPRESS_PHPUNIT_TIMEOUT_SECONDS: '1',
      HOMEBOY_TEST_FAILURES_FILE: path.join(root, 'test-failures.json'),
      FIXTURE_PIDS_PATH: path.join(root, 'pids.json'),
    },
    encoding: 'utf8',
    timeout: 15000,
  });
  assert.equal(run.status, 124, run.stderr);
  assert.ok(Date.now() - startedAt < 10000, 'timeout return must remain bounded while descendant holds inherited pipes');
  assert.match(run.stdout, /"schema":"homeboy\/wp-codebox-timeout-diagnostics\/v1"/);
  assert.doesNotMatch(run.stdout, /fixture-bearer-secret|fixture-cookie-secret|fixture-url-secret|"131071"/);

  const runDirectory = (await readdir(artifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.'));
  const runtimeFiles = path.join(artifacts, runDirectory, 'runtime-fixture', 'files');
  const timeoutEvidence = JSON.parse(await readFile(path.join(runtimeFiles, 'wp-codebox-timeout-diagnostics.json'), 'utf8'));
  assert.equal(timeoutEvidence.phase, 'wp-codebox-phpunit-recipe-run');
  assert.equal(timeoutEvidence.budget_seconds, 1);
  assert.equal(timeoutEvidence.execution.last.stdout.kind, 'byte_map_omitted');
  assert.equal(timeoutEvidence.execution.count_complete, false);
  assert.doesNotMatch(JSON.stringify(timeoutEvidence), /fixture-bearer-secret|fixture-cookie-secret|fixture-url-secret|"131071"/);

  const payload = await readFile(path.join(runtimeFiles, 'recipe-run.json'), 'utf8');
  assert.equal(payload, '[REDACTED OVERLONG LINE]\n');
  assert.doesNotMatch(payload, /fixture-bearer-secret|fixture-cookie-secret|fixture-url-secret/);
  const manifest = JSON.parse(await readFile(path.join(invocationArtifacts, 'homeboy-artifact-manifest.json'), 'utf8'));
  assert.ok(manifest.artifacts.some((artifact) => artifact.kind === 'wp-codebox-timeout-diagnostics'));
  assert.ok(manifest.artifacts.some((artifact) => artifact.kind === 'recipe-run-payload'));
  assert.ok(Buffer.byteLength(await readFile(path.join(runtimeFiles, 'phpunit-output.log'))) <= 8192);
  const diagnosis = JSON.parse(await readFile(path.join(runtimeFiles, 'phpunit-execution-diagnosis.json'), 'utf8'));
  assert.ok(diagnosis.stage_markers[0].length <= 128 * 1024, 'timeout diagnosis must not materialize the multi-megabyte stage log');
  const pids = JSON.parse(await readFile(path.join(root, 'pids.json'), 'utf8'));
  assert.equal(await waitForReaped(pids.descendant), true, 'TERM-ignoring descendant must be reaped');
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit timeout diagnostics smoke passed.');
