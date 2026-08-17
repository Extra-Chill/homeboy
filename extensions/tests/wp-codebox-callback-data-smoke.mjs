#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { skipUnlessWpCodeboxCanonicalContract } from './lib/wp-codebox-runtime-contract-availability.mjs';

skipUnlessWpCodeboxCanonicalContract('wp-codebox callback data smoke');

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const runnerPath = path.join(repoRoot, 'agent-runtimes/wp-codebox/scripts/agent/homeboy-wp-codebox-task-runner.cjs');
const runnerSource = fs.readFileSync(runnerPath, 'utf8');
const callbackHelperSource = runnerSource.slice(
  runnerSource.indexOf('function homeboyCallbackDataPluginSource()'),
  runnerSource.indexOf('function writeHomeboyCallbackDataPlugin')
);

assert.match(callbackHelperSource, /homeboy_callback_data_get/);
assert.match(callbackHelperSource, /homeboy_callback_data_set/);
assert.match(callbackHelperSource, /homeboy_callback_data_append/);
assert.match(callbackHelperSource, /homeboy_callback_data_output_event/);
assert.equal(/Data Machine|DataMachine|datamachine|data-machine|wp-site-generator|WPSG|site-generator|site generator/.test(callbackHelperSource), false);
assert.equal(/datamachine_(?:merge|get)_engine_data/.test(runnerSource), false);

const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-callback-data-'));
const fakeBin = path.join(tempRoot, 'fake-wp-codebox.cjs');
const artifactRoot = path.join(tempRoot, 'artifacts');

fs.writeFileSync(fakeBin, `#!/usr/bin/env node
if (process.argv.includes('--version')) {
  process.stdout.write('0.21.0');
  process.exit(0);
}
const fs = require('node:fs');
const inputArg = process.argv.find((arg) => arg.startsWith('--input-file='));
const input = JSON.parse(fs.readFileSync(inputArg.slice('--input-file='.length), 'utf8'));
const taskInput = input.task_input;
const callbackPath = taskInput.runtime_env.HOMEBOY_CALLBACK_DATA_PATH;
const helper = taskInput.extra_plugins.find((plugin) => plugin.metadata?.source === 'homeboy-runtime-callback-data');
if (!callbackPath || !helper || !fs.existsSync(callbackPath)) {
  throw new Error('callback data helper was not injected');
}
const initial = JSON.parse(fs.readFileSync(callbackPath, 'utf8'));
fs.writeFileSync(callbackPath, JSON.stringify({
  data: { ...initial.data, packet_url: 'https://example.com/packet.json', count: 2 },
  events: [{ schema: 'homeboy/runtime-callback-event/v1', name: 'packet.ready', payload: { id: 'packet-1' } }],
}, null, 2));
const agentResult = { outputs: { direct_output: 'ok' } };
const execution = {
  recipeCommand: 'wp-codebox.agent-sandbox-run',
  exitCode: 0,
  stdout: JSON.stringify({ status: 'completed', output: JSON.stringify(agentResult) }),
};
process.stdout.write(JSON.stringify({
  schema: 'wp-codebox/agent-task-run/v1',
  success: true,
  status: 'completed',
  run: { agentResult },
  executions: [execution],
}));
`);

const request = {
  schema: 'wp-codebox/task-input/v1',
  goal: 'Exercise callback data.',
  wp_codebox_bin: fakeBin,
  artifacts_path: artifactRoot,
  runtime_task: { ability: 'example/callback-data', input: {} },
  callback_data: { initial: { seed: 'starter' } },
};

const result = spawnSync(process.execPath, [runnerPath], {
  cwd: repoRoot,
  env: {
    ...process.env,
    HOMEBOY_WP_CODEBOX_TASK_REQUEST: JSON.stringify(request),
  },
  encoding: 'utf8',
});

assert.equal(result.status, 0, `runner failed\nSTDOUT:\n${result.stdout}\nSTDERR:\n${result.stderr}`);
const payload = JSON.parse(result.stdout);
assert.equal(payload.outputs.direct_output, 'ok');
assert.deepEqual(payload.outputs.callback_data, {
  seed: 'starter',
  packet_url: 'https://example.com/packet.json',
  count: 2,
});
assert.equal(payload.outputs.callback_events[0].name, 'packet.ready');
assert.equal(payload.metadata.callback_data.packet_url, 'https://example.com/packet.json');
assert.equal(payload.artifacts.some((artifact) => artifact.kind === 'runtime-callback-data'), true);
assert.equal(payload.evidence_refs.some((ref) => ref.kind === 'runtime-callback-data'), true);

fs.rmSync(tempRoot, { recursive: true, force: true });
console.log('wp-codebox callback data smoke passed');
