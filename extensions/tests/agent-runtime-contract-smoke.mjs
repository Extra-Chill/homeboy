#!/usr/bin/env node
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const runtimeDir = path.join(rootDir, 'agent-runtimes', 'fake-runtime');
const manifestPath = path.join(runtimeDir, 'fake-runtime.json');
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
const { expandAgentTaskToolPresets } = require(path.join(rootDir, 'agent-runtimes', 'lib', 'agent-task-provider-contract.js'));

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
assert.deepEqual(provider.tool_presets, ['runner_workspace', 'publication']);
const expandedToolPresets = expandAgentTaskToolPresets(provider.tool_presets);
assert.deepEqual(provider.workspace_tools, expandedToolPresets.workspace_tools);
assert.deepEqual(provider.publication_tools, expandedToolPresets.publication_tools);
assert.equal(provider.workspace_materialization.cwd, 'git_checkout');
assert.equal(provider.workspace_materialization.requires_git, true);
assert.equal(provider.workspace_materialization.write_scope, 'artifacts');
assert.deepEqual(provider.workspace_materialization.artifact_paths, ['.homeboy/fake-runtime']);
assert.deepEqual(provider.secret_requirements, []);
assert.deepEqual(provider.secret_env_requirements, []);
assert.deepEqual(provider.provider_defaults['fake-runtime'].secret_env, []);
assert.ok(provider.diagnostics.outcome_fields.includes('diagnostics'));
assert.deepEqual(provider.diagnostics.artifact_kinds, ['fake-runtime-outcome', 'fake-runtime-transcript']);

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

const workspaceDir = mkdtempSync(path.join(os.tmpdir(), 'homeboy-fake-runtime-'));
const result = spawnSync('node', [path.join(runtimeDir, 'scripts', 'agent', 'fake-agent-task-executor.cjs')], {
	input: JSON.stringify(request),
	encoding: 'utf8',
	cwd: workspaceDir,
});

assert.equal(result.status, 0, result.stderr);
assert.equal(result.stderr, '', 'fake provider should not write stderr on success');

const outcome = JSON.parse(result.stdout);
assert.equal(outcome.schema, provider.outcome_schema);
assert.equal(outcome.task_id, request.task_id);
assert.ok(provider.outcome_statuses.includes(outcome.status));
assert.equal(outcome.status, 'succeeded');
assert.equal(outcome.metadata.provider, provider.backend);
assert.deepEqual(outcome.artifacts.map((artifact) => artifact.kind), ['fake-runtime-outcome', 'fake-runtime-transcript']);

const writtenOutcome = JSON.parse(readFileSync(path.join(workspaceDir, '.homeboy', 'fake-runtime', 'outcome.json'), 'utf8'));
const writtenTranscript = readFileSync(path.join(workspaceDir, '.homeboy', 'fake-runtime', 'transcript.log'), 'utf8');
assert.deepEqual(writtenOutcome, outcome);
assert.match(writtenTranscript, /task_id=fake-runtime-contract-smoke/);
assert.match(writtenTranscript, /status=succeeded/);
rmSync(workspaceDir, { recursive: true, force: true });

console.log('agent runtime contract smoke passed');
