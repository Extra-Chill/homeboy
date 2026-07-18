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
	CODEX_DEFAULT_COMMAND,
	CODEX_DEFAULT_COMMAND_ARGS,
	CODEX_SECRET_ENV,
	executeCodexAgentTask,
	providerContract,
} = require('..');
const {
	resolveRuntimeProvider,
	runtimeRegistry,
} = require('../../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');

const runtimeRoot = path.join(__dirname, '..');
const repoRoot = path.join(__dirname, '..', '..', '..');

function secretEnvRequirementForProvider(contract, provider) {
	return contract.secret_env_requirements.find((requirement) => (
		requirement.when.any.some((selector) => selector.path === 'executor.config.provider' && selector.equals === provider)
	));
}

const provider = providerContract();
assert.equal(provider.id, 'codex.agent-task-executor');
assert.equal(provider.backend, 'codex');
assert.equal(provider.runtime, 'codex');
assert.equal(provider.status, 'available');
assert.equal(provider.integration_contract, 'homeboy-codex-agent-task/v1');
assert.equal(Object.hasOwn(provider.lifecycle, 'max_concurrency_default'), false);
assert.equal(provider.lifecycle.cancellation, 'provider_signal');
assert.deepEqual(secretEnvRequirementForProvider(provider, 'codex').env, CODEX_SECRET_ENV);
assert.deepEqual(provider.provider_defaults.codex.secret_env, CODEX_SECRET_ENV);
assert.equal(provider.provider_defaults.codex.model, 'gpt-5.5');
assert.equal(provider.provider_defaults.codex.command, CODEX_DEFAULT_COMMAND);
assert.deepEqual(provider.provider_defaults.codex.command_args, CODEX_DEFAULT_COMMAND_ARGS);
assert.equal(provider.redacted_metadata_keys.includes('codex_auth'), true);
assert.equal(provider.capabilities.includes('repo_workspace'), true);
assert.equal(provider.capabilities.includes('patch_artifacts'), true);
assert.equal(provider.capabilities.includes('nested_orchestrator'), false);
assert.equal(provider.capabilities.includes('wordpress_sandbox'), false);

const manifest = JSON.parse(fs.readFileSync(path.join(runtimeRoot, 'codex.json'), 'utf8'));
assert.equal(manifest.id, 'codex');
assert.equal(manifest.name, 'Codex');
assert.equal(manifest.agent_task_executors.length, 1);

const registry = runtimeRegistry({ repoRoot });
assert.equal(registry.codex.id, 'codex');
const resolvedRuntime = resolveRuntimeProvider('codex', { repoRoot, registry });
assert.equal(resolvedRuntime.id, 'codex');
assert.equal(resolvedRuntime.executor.backend, 'codex');
assert.equal(
	resolvedRuntime.executor.path,
	path.join(repoRoot, 'agent-runtimes', 'codex', 'scripts', 'agent', 'homeboy-codex-agent-task-executor.cjs')
);

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codex-provider-contract-'));
try {
	const runtimesRoot = path.join(root, 'agent-runtimes');
	const runtimePath = path.join(runtimesRoot, 'codex');
	fs.mkdirSync(runtimesRoot, { recursive: true });
	fs.symlinkSync(runtimeRoot, runtimePath, 'dir');

	const [program, scriptTemplate] = provider.invocation.argv;
	assert.equal(program, 'node');
	const scriptPath = scriptTemplate.replaceAll('{{runtime_path}}', runtimePath);
	assert.equal(
		path.normalize(scriptPath),
		path.join(runtimePath, 'scripts', 'agent', 'homeboy-codex-agent-task-executor.cjs')
	);
	assert.equal(fs.existsSync(scriptPath), true, `provider command target should exist: ${scriptPath}`);

	const mockCliPath = path.join(root, 'mock-codex.cjs');
	fs.writeFileSync(mockCliPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
assert.equal(process.argv[2], 'exec');
assert.equal(process.argv[3], '--model');
assert.equal(process.argv[4], 'gpt-5.5');
assert.equal(process.argv.at(-1), 'Prove the Codex runtime boundary without leaking secrets.');
assert.equal(process.env.UNDECLARED_SECRET, undefined);
process.stdout.write(process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN || 'missing secret');
process.stderr.write(process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN || 'missing secret');
process.exit(0);
`);

	const contractResult = spawnSync(process.execPath, [scriptPath, '--provider-contract'], { encoding: 'utf8' });
	assert.equal(contractResult.status, 0, contractResult.stderr);
	assert.deepEqual(JSON.parse(contractResult.stdout), manifest.agent_task_executors[0]);

	const request = {
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'codex-real-executor',
			executor: {
			backend: 'codex',
			runtime: 'codex',
			config: {
				provider: 'codex',
				model: 'gpt-5.5',
				command: process.execPath,
				command_args: [mockCliPath, 'exec'],
				artifacts_path: path.join(root, 'artifacts'),
			},
		},
		instructions: 'Prove the Codex runtime boundary without leaking secrets.',
	};
	const runResult = spawnSync(process.execPath, [scriptPath], {
		encoding: 'utf8',
		env: {
			...process.env,
			AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'refresh-token-must-not-leak',
			AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'access-token-must-not-leak',
			UNDECLARED_SECRET: 'must-not-reach-codex',
		},
		input: JSON.stringify(request),
	});
	assert.equal(runResult.status, 0, runResult.stderr);
	const parsedRun = JSON.parse(runResult.stdout);
	const previousRefreshToken = process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN;
	const previousAccessToken = process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN;
	process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN = 'refresh-token-must-not-leak';
	process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN = 'access-token-must-not-leak';
	try {
		assert.deepEqual(parsedRun, executeCodexAgentTask(request));
	} finally {
		if (previousRefreshToken === undefined) {
			delete process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN;
		} else {
			process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN = previousRefreshToken;
		}
		if (previousAccessToken === undefined) {
			delete process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN;
		} else {
			process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN = previousAccessToken;
		}
	}
	assert.equal(parsedRun.artifacts.some((artifact) => artifact.stream === 'stdout'), true);
	assert.equal(parsedRun.artifacts.some((artifact) => artifact.stream === 'stderr'), true);
	for (const artifact of parsedRun.artifacts) {
		const content = fs.readFileSync(artifact.path, 'utf8');
		assert.equal(content.includes('refresh-token-must-not-leak'), false);
		assert.equal(content.includes('access-token-must-not-leak'), false);
	}
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('refresh-token-must-not-leak'), false);
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('access-token-must-not-leak'), false);
	assert.equal(executeCodexAgentTask({ ...request, executor: { backend: 'codex', runtime: 'wrong', config: {} } }).failure_code, 'agent_task.invalid_codex_request');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('Codex agent task executor boundary passed\n');
