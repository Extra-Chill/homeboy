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
	OPENCODE_INVOCATION,
	OPENCODE_PROVIDER_DEFAULTS,
	OPENCODE_PROVIDER_PREFLIGHT,
	OPENCODE_ROLE_ALIASES,
	OPENCODE_RUNNER_READINESS,
	OPENCODE_SECRET_ENV,
	OPENCODE_WORKSPACE_MATERIALIZATION,
	OPENCODE_WORKSPACE_TOOLS,
	executeOpenCodeAgentTask,
	providerContract,
} = require('..');

const runtimeRoot = path.join(__dirname, '..');

function secretEnvRequirementForProvider(contract, provider) {
	return contract.secret_env_requirements.find((requirement) => (
		requirement.when.any.some((selector) => selector.path === 'executor.config.provider' && selector.equals === provider)
	));
}

const provider = providerContract();
assert.equal(provider.id, 'opencode.agent-task-executor');
assert.equal(provider.backend, 'opencode');
assert.equal(provider.runtime_id, 'opencode');
assert.equal(provider.status, 'active');
assert.equal(provider.integration_contract, 'homeboy-opencode-agent-task/v1');
assert.deepEqual(provider.invocation, OPENCODE_INVOCATION);
assert.equal(provider.lifecycle.max_concurrency_default, 1);
assert.equal(provider.lifecycle.cancellation, 'provider_signal');
assert.deepEqual(secretEnvRequirementForProvider(provider, 'codex').env, OPENCODE_SECRET_ENV);
assert.deepEqual(provider.provider_defaults.codex.secret_env, OPENCODE_SECRET_ENV);
assert.equal(provider.provider_defaults.codex.model, 'gpt-5.5');
assert.deepEqual(provider.provider_defaults.codex.secret_env_sources, OPENCODE_PROVIDER_DEFAULTS.codex.secret_env_sources);
assert.deepEqual(provider.provider_preflight, OPENCODE_PROVIDER_PREFLIGHT);
assert.deepEqual(provider.runner_readiness, OPENCODE_RUNNER_READINESS);
assert.deepEqual(provider.workspace_materialization, OPENCODE_WORKSPACE_MATERIALIZATION);
assert.deepEqual(provider.workspace_tools, OPENCODE_WORKSPACE_TOOLS);
assert.deepEqual(provider.role_aliases, OPENCODE_ROLE_ALIASES);
assert.equal(provider.redacted_metadata_keys.includes('opencode_auth'), true);
assert.equal(provider.capabilities.includes('repo_workspace'), true);
assert.equal(provider.capabilities.includes('patch_artifacts'), true);
assert.equal(provider.capabilities.includes('browser_runtime'), false);

const manifest = JSON.parse(fs.readFileSync(path.join(runtimeRoot, 'opencode.json'), 'utf8'));
assert.equal(manifest.id, 'opencode');
assert.equal(manifest.name, 'OpenCode');
assert.equal(manifest.agent_task_executors.length, 1);
assert.equal(manifest.agent_task_executors[0].capabilities.includes('nested_orchestrator'), true);
assert.deepEqual(manifest.agent_task_executors[0], providerContract());

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opencode-provider-contract-'));
try {
	const runtimesRoot = path.join(root, 'agent-runtimes');
	const runtimePath = path.join(runtimesRoot, 'opencode');
	fs.mkdirSync(runtimesRoot, { recursive: true });
	fs.symlinkSync(runtimeRoot, runtimePath, 'dir');

	const command = provider.command.replaceAll('{{runtime_path}}', runtimePath);
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

	const request = {
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'opencode-real-executor',
		executor: {
			backend: 'opencode',
			runtime: 'opencode',
			config: {
				provider: 'codex',
				runtime_bin: process.execPath,
				command_args: [mockCliPath],
			},
		},
		instructions: 'Prove the OpenCode provider boundary without leaking secrets.',
	};
	const runResult = spawnSync(process.execPath, [scriptPath], {
		encoding: 'utf8',
		env: {
			...process.env,
			AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'refresh-token-must-not-leak',
			AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'access-token-must-not-leak',
		},
		input: JSON.stringify(request),
	});
	assert.equal(runResult.status, 0, runResult.stderr);
	assert.deepEqual(JSON.parse(runResult.stdout), executeOpenCodeAgentTask(request));
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('refresh-token-must-not-leak'), false);
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('access-token-must-not-leak'), false);
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('OpenCode agent task executor boundary passed\n');
