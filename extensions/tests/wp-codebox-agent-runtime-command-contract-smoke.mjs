#!/usr/bin/env node
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const manifestPath = path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'wp-codebox.json');
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
const provider = manifest.agent_task_executors[0];

assert.equal(provider.schema, 'homeboy/agent-task-executor-provider/v1');
assert.equal(provider.invocation.schema, 'homeboy/command-invocation/v1');
assert.deepEqual(provider.invocation.argv, [
	'node',
	'{{runtime_path}}/scripts/agent/homeboy-codebox-agent-task-executor.cjs',
]);
assert.equal(
	provider.invocation.display,
	'node {{runtime_path}}/scripts/agent/homeboy-codebox-agent-task-executor.cjs'
);
assert.equal(provider.command, provider.invocation.display, 'legacy command stays display-compatible during deprecation');

console.log('wp-codebox agent runtime command contract smoke passed');
