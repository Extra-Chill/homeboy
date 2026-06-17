#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const runtimeDir = path.join(rootDir, 'agent-runtimes', 'fake-runtime');
const manifestPath = path.join(runtimeDir, 'fake-runtime.json');
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));

const requiredRootFields = ['schema', 'id', 'name', 'version', 'description', 'agent_task_executors'];
for (const field of requiredRootFields) {
	assert.ok(manifest[field], `missing root manifest field: ${field}`);
}

assert.ok(Array.isArray(manifest.agent_task_executors), 'agent_task_executors must be an array');
assert.equal(manifest.schema, 'homeboy/agent-runtime-manifest/v1');
assert.equal(manifest.id, 'fake-runtime');
assert.equal(manifest.agent_task_executors.length, 1, 'fake runtime should expose one executor');

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
assert.equal(provider.workspace_materialization.cwd, 'git_checkout');
assert.equal(provider.workspace_materialization.requires_git, true);
assert.equal(provider.workspace_materialization.write_scope, 'artifacts');
assert.deepEqual(provider.workspace_materialization.artifact_paths, ['.homeboy/fake-runtime']);
assert.ok(provider.secret_requirements.some((secret) => secret.name === 'FAKE_RUNTIME_TOKEN'));
assert.deepEqual(provider.secret_env_requirements[0].env, ['FAKE_RUNTIME_TOKEN']);
assert.deepEqual(provider.provider_defaults['fake-runtime'].secret_env, ['FAKE_RUNTIME_TOKEN']);
assert.ok(provider.diagnostics.outcome_fields.includes('diagnostics'));

const command = provider.command.replace('{{runtime_path}}', runtimeDir);
assert.equal(command, `node ${runtimeDir}/scripts/agent/fake-agent-task-executor.cjs`);

const request = {
	schema: provider.request_schema,
	task_id: 'fake-runtime-contract-smoke',
	executor: {
		backend: provider.backend,
	},
	instructions: 'Validate the generic runtime provider command contract.',
};

const result = spawnSync('node', [path.join(runtimeDir, 'scripts', 'agent', 'fake-agent-task-executor.cjs')], {
	input: JSON.stringify(request),
	encoding: 'utf8',
	env: {
		...process.env,
		FAKE_RUNTIME_TOKEN: 'redacted-fixture-token',
	},
});

assert.equal(result.status, 0, result.stderr);
assert.equal(result.stderr, '', 'fake provider should not write stderr on success');

const outcome = JSON.parse(result.stdout);
assert.equal(outcome.schema, provider.outcome_schema);
assert.equal(outcome.task_id, request.task_id);
assert.ok(provider.outcome_statuses.includes(outcome.status));
assert.equal(outcome.status, 'succeeded');
assert.equal(outcome.metadata.provider, provider.backend);
assert.deepEqual(outcome.metadata.secret_env_names, ['FAKE_RUNTIME_TOKEN']);
assert.doesNotMatch(result.stdout, /redacted-fixture-token/, 'provider outcome leaked secret value');

console.log('agent runtime contract smoke passed');
