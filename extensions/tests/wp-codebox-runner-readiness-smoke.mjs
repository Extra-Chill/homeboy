import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const readiness = path.join(root, 'agent-runtimes/wp-codebox/scripts/agent/homeboy-wp-codebox-runner-readiness.cjs');
const manifest = JSON.parse(fs.readFileSync(path.join(root, 'agent-runtimes/wp-codebox/wp-codebox.json'), 'utf8'));
const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'hbe-wp-codebox-runner-readiness-'));
const run = (env) => {
  const result = spawnSync(process.execPath, [readiness], { encoding: 'utf8', env: { ...process.env, ...env } });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
};
const executable = (file, version) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `#!/usr/bin/env node\nprocess.stdout.write(${JSON.stringify(version)});\n`);
  fs.chmodSync(file, 0o755);
};

try {
  const declaration = manifest.agent_task_executors[0].runner_readiness[0];
  assert.deepEqual(declaration.invocation.argv, ['node', '{{runtime_path}}/scripts/agent/homeboy-wp-codebox-runner-readiness.cjs']);
  assert.equal(declaration.remediation, 'homeboy extension setup wordpress');
  const stalePath = path.join(temp, 'stale-path');
  const stale = path.join(stalePath, 'wp-codebox');
  executable(stale, '0.12.27');
  const incomplete = path.join(temp, 'incomplete');
  fs.mkdirSync(path.join(incomplete, 'source/packages/cli'), { recursive: true });

  const dangling = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete, HOMEBOY_WP_CODEBOX_BIN: path.join(temp, 'missing'), PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(dangling.ready, false);
  assert.equal(dangling.classification, 'configured_binary_missing');
  assert.equal(dangling.remediation, 'homeboy extension setup wordpress');

  const incompleteResult = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete, HOMEBOY_WP_CODEBOX_BIN: '', PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(incompleteResult.ready, false);
  assert.equal(incompleteResult.classification, 'managed_cache_incomplete');
  assert.match(incompleteResult.reason, /built CLI entrypoint is missing/);

  const healthy = path.join(temp, 'healthy');
  const managedCli = path.join(healthy, 'source/packages/cli/dist/index.js');
  executable(managedCli, '0.19.0');
  const managed = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: healthy, HOMEBOY_WP_CODEBOX_BIN: '', PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(managed.ready, true);
  assert.equal(managed.identity.executable, managedCli);
  assert.equal(managed.identity.source, 'resolved');
  assert.equal(managed.identity.version, '0.19.0');

  const override = path.join(temp, 'override');
  executable(override, 'external-1.0');
  const explicit = run({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: incomplete, HOMEBOY_WP_CODEBOX_BIN: override, PATH: `${stalePath}:${process.env.PATH}` });
  assert.equal(explicit.ready, true);
  assert.equal(explicit.identity.executable, override);
  assert.equal(explicit.identity.source, 'explicit_override');
  assert.equal(explicit.identity.version, 'external-1.0');
} finally {
  fs.rmSync(temp, { recursive: true, force: true });
}

console.log('wp-codebox runner readiness smoke passed');
