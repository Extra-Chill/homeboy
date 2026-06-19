#!/usr/bin/env node
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const manifestPath = path.join(rootDir, 'tests', 'fixtures', 'agent-runtime-manifest.json');
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
const { expandAgentTaskToolPresets } = require(path.join(rootDir, 'agent-runtimes', 'lib', 'agent-task-provider-contract.js'));

const requiredRootFields = ['schema', 'id', 'name', 'version', 'description', 'agent_task_executors'];
for (const field of requiredRootFields) {
	assert.ok(manifest[field], `missing root manifest field: ${field}`);
}

assert.ok(Array.isArray(manifest.agent_task_executors), 'agent_task_executors must be an array');
assert.equal(manifest.schema, 'homeboy/agent-runtime-manifest/v1');
assert.equal(manifest.id, 'fixture-runtime');
assert.equal(manifest.agent_task_executors.length, 1, 'fixture manifest should expose one executor');

const provider = manifest.agent_task_executors[0];
const requiredProviderFields = [
	'schema',
	'id',
	'label',
	'backend',
	'command',
	'request_schema',
	'outcome_schema',
	'request_required_fields',
	'outcome_statuses',
	'failure_classifications',
	'redacted_metadata_keys',
	'capabilities',
	'runner_readiness',
	'role_aliases',
	'tool_presets',
	'workspace_tools',
	'publication_tools',
	'workspace_materialization',
	'secret_requirements',
	'secret_env_requirements',
	'provider_defaults',
	'diagnostics',
	'status',
	'integration_contract',
];

for (const field of requiredProviderFields) {
	assert.ok(provider[field], `missing provider field: ${field}`);
}

assert.equal(provider.schema, 'homeboy/agent-task-executor-provider/v1');
assert.equal(provider.request_schema, 'homeboy/agent-task-request/v1');
assert.equal(provider.outcome_schema, 'homeboy/agent-task-outcome/v1');
assert.match(provider.command, /\{\{runtime_path\}\}/, 'provider command must use runtime_path interpolation');
assert.ok(provider.request_required_fields.includes('executor.backend'));
assert.ok(provider.outcome_statuses.includes('succeeded'));
assert.ok(provider.failure_classifications.includes('request_validation'));
assert.ok(provider.redacted_metadata_keys.includes('secrets'));
assert.ok(provider.capabilities.includes('structured_outcome'));
assert.deepEqual(provider.runner_readiness, []);
assert.deepEqual(provider.role_aliases.artifact_kinds.patch, ['fixture-runtime-patch']);
assert.deepEqual(provider.tool_presets, ['runner_workspace', 'publication']);
const expandedToolPresets = expandAgentTaskToolPresets(provider.tool_presets);
assert.deepEqual(provider.workspace_tools, expandedToolPresets.workspace_tools);
assert.deepEqual(provider.publication_tools, expandedToolPresets.publication_tools);
assert.equal(provider.workspace_materialization.cwd, 'git_checkout');
assert.equal(provider.workspace_materialization.requires_git, true);
assert.equal(provider.workspace_materialization.write_scope, 'artifacts');
assert.deepEqual(provider.workspace_materialization.artifact_paths, ['.homeboy/fixture-runtime']);
assert.deepEqual(provider.secret_requirements, []);
assert.deepEqual(provider.secret_env_requirements, []);
assert.deepEqual(provider.provider_defaults['fixture-runtime'].secret_env, []);
assert.ok(provider.diagnostics.outcome_fields.includes('diagnostics'));
assert.deepEqual(provider.diagnostics.artifact_kinds, ['fixture-runtime-outcome', 'fixture-runtime-transcript']);
assert.match(provider.command, /\{\{runtime_path\}\}/, 'provider command must be runtime-relative without shipping a fixture runtime package');

console.log('agent runtime contract smoke passed');
