#!/usr/bin/env node
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const manifestPath = path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'wp-codebox.json');
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
const provider = manifest.agent_task_executors[0];
const { providerMetadata, providerMetadataManifest } = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox'));

assert.equal(provider.schema, 'homeboy/agent-task-executor-provider/v1');
assert.equal(provider.invocation.schema, 'homeboy/command-invocation/v1');
assert.equal(provider.backend, 'codebox');
assert.equal(provider.runtime_id, 'wp-codebox');
assert.deepEqual(provider.invocation.argv, [
	'node',
	'{{runtime_path}}/scripts/agent/homeboy-codebox-agent-task-executor.cjs',
]);
assert.equal(
	provider.invocation.display,
	'node {{runtime_path}}/scripts/agent/homeboy-codebox-agent-task-executor.cjs'
);
assert.equal(provider.command, provider.invocation.display, 'legacy command stays display-compatible during deprecation');
assert.equal(provider.provider_metadata.schema, 'homeboy/agent-task-provider-metadata/v1');
assert.equal(provider.provider_metadata.selection.backend, 'codebox');
assert.equal(provider.provider_metadata.selection.runtime_id, 'wp-codebox');
assert.deepEqual(provider.provider_metadata.selection.provider_id_paths, [
	'executor.config.provider',
	'executor.provider',
	'provider',
]);
assert.deepEqual(provider.provider_metadata.selection.model_paths, [
	'executor.model',
	'executor.config.model',
	'model',
]);
assert.deepEqual(
	provider.provider_metadata.providers.map((entry) => entry.id),
	['openai', 'codex', 'claude-code']
);
assert.equal(providerMetadataManifest().schema, 'homeboy/agent-task-provider-metadata/v1');
assert.equal(providerMetadata('codex').backend, 'codebox');
assert.equal(providerMetadata('codex').runtime_id, 'wp-codebox');
assert.match(providerMetadata('codex').model_guidance, /executor\.config\.model/);

console.log('wp-codebox agent runtime command contract smoke passed');
