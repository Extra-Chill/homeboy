'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
	OPENCODE_SECRET_ENV,
	executeOpenCodeAgentTask,
	providerContract,
} = require('../../agent-runtimes/opencode');

function secretEnvRequirementForProvider(contract, provider) {
	return contract.secret_env_requirements.find((requirement) => (
		requirement.when.any.some((selector) => selector.path === 'executor.config.provider' && selector.equals === provider)
	));
}

const provider = providerContract();
assert.equal(provider.id, 'opencode.agent-task-executor');
assert.equal(provider.backend, 'opencode');
assert.equal(provider.status, 'available');
assert.equal(provider.integration_contract, 'homeboy-opencode-agent-task/v1');
assert.equal(provider.lifecycle.max_concurrency_default, 1);
assert.equal(provider.lifecycle.cancellation, 'provider_signal');
assert.deepEqual(secretEnvRequirementForProvider(provider, 'codex').env, OPENCODE_SECRET_ENV);
assert.deepEqual(provider.provider_defaults.codex.secret_env, OPENCODE_SECRET_ENV);
assert.equal(provider.provider_defaults.codex.model, 'gpt-5.5');
assert.equal(provider.redacted_metadata_keys.includes('opencode_auth'), true);
assert.equal(provider.capabilities.includes('repo_workspace'), true);
assert.equal(provider.capabilities.includes('patch_artifacts'), true);
assert.equal(provider.capabilities.includes('browser_runtime'), false);

const manifest = JSON.parse(fs.readFileSync(path.join(__dirname, '..', '..', 'agent-runtimes', 'opencode', 'opencode.json'), 'utf8'));
assert.equal(manifest.id, 'opencode');
assert.equal(manifest.name, 'OpenCode');
assert.equal(manifest.agent_task_executors.length, 1);
assert.equal(manifest.agent_task_executors[0].capabilities.includes('nested_orchestrator'), true);
assert.deepEqual(manifest.agent_task_executors[0], providerContract());

const runtime = {
  agent_task_executors: [providerContract({
    command: 'node {{runtime_path}}/scripts/agent/homeboy-opencode-agent-task-executor.cjs',
  })],
};

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opencode-provider-contract-'));
try {
	const runtimesRoot = path.join(root, 'agent-runtimes');
	const runtimePath = path.join(runtimesRoot, 'opencode');
	fs.mkdirSync(runtimesRoot, { recursive: true });
	fs.symlinkSync(path.join(__dirname, '..', '..', 'agent-runtimes', 'opencode'), runtimePath, 'dir');

	const command = runtime.agent_task_executors[0].command.replaceAll('{{runtime_path}}', runtimePath);
	const [, scriptPath] = command.match(/^node\s+(.+)$/) || [];
	assert(scriptPath, 'provider command should be a node script command');
	assert.equal(
		path.normalize(scriptPath),
		path.join(runtimePath, 'scripts', 'agent', 'homeboy-opencode-agent-task-executor.cjs')
	);
	assert.equal(fs.existsSync(scriptPath), true, `provider command target should exist: ${scriptPath}`);

	const mockCliPath = path.join(root, 'mock-opencode.cjs');
	fs.writeFileSync(mockCliPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
assert.equal(process.argv[2], 'run');
assert.equal(process.argv.at(-1), 'Prove the OpenCode provider boundary without leaking secrets.');
process.stdout.write(process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN || 'missing secret');
process.stderr.write(process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN || 'missing secret');
process.exit(0);
`);

	const contractResult = spawnSync(process.execPath, [scriptPath, '--provider-contract'], { encoding: 'utf8' });
	assert.equal(contractResult.status, 0, contractResult.stderr);
	assert.deepEqual(JSON.parse(contractResult.stdout), providerContract());

	const runResult = spawnSync(process.execPath, [scriptPath], {
		encoding: 'utf8',
		env: {
			...process.env,
			AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'refresh-token-must-not-leak',
			AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'access-token-must-not-leak',
		},
		input: JSON.stringify({
			schema: 'homeboy/agent-task-request/v1',
			task_id: 'opencode-real-executor',
			executor: {
				backend: 'opencode',
				config: {
					provider: 'codex',
					command: process.execPath,
					command_args: [mockCliPath],
				},
			},
			instructions: 'Prove the OpenCode provider boundary without leaking secrets.',
		}),
	});
	assert.equal(runResult.status, 0, runResult.stderr);
	assert.deepEqual(JSON.parse(runResult.stdout), executeOpenCodeAgentTask({
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'opencode-real-executor',
		executor: {
			backend: 'opencode',
			config: {
				provider: 'codex',
				command: process.execPath,
				command_args: [mockCliPath],
			},
		},
		instructions: 'Prove the OpenCode provider boundary without leaking secrets.',
	}));
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('refresh-token-must-not-leak'), false);
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('access-token-must-not-leak'), false);
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('OpenCode agent task executor boundary passed\n');
