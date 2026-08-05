'use strict';

require('../../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');

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
	CLAUDE_CODE_OPTIONAL_SECRET_ENV,
	CLAUDE_CODE_REQUIRED_SECRET_ENV,
	CLAUDE_CODE_SECRET_ENV,
	executeClaudeCodeAgentTask,
	providerContract,
} = require('..');

const runtimeRoot = path.join(__dirname, '..');

function secretEnvRequirementForProvider(contract, provider) {
	return contract.secret_env_requirements.find((requirement) => (
		requirement.when.any.some((selector) => selector.path === 'executor.config.provider' && selector.equals === provider)
	));
}

const provider = providerContract();
assert.equal(provider.id, 'claude-code.agent-task-executor');
assert.equal(provider.backend, 'claude-code');
assert.equal(provider.runtime, 'claude-code');
assert.equal(provider.status, 'available');
assert.equal(provider.integration_contract, 'homeboy-claude-code-agent-task/v1');
assert.equal(Object.hasOwn(provider.lifecycle, 'max_concurrency_default'), false);
assert.equal(provider.lifecycle.cancellation, 'provider_signal');
assert.deepEqual(secretEnvRequirementForProvider(provider, 'claude-code').env, CLAUDE_CODE_REQUIRED_SECRET_ENV);
assert.deepEqual(provider.provider_defaults['claude-code'].secret_env, CLAUDE_CODE_SECRET_ENV);
assert.deepEqual(provider.provider_defaults['claude-code'].required_secret_env, CLAUDE_CODE_REQUIRED_SECRET_ENV);
assert.deepEqual(provider.provider_defaults['claude-code'].optional_secret_env, CLAUDE_CODE_OPTIONAL_SECRET_ENV);
assert.deepEqual(provider.provider_preflight['claude-code'].required_secret_env, CLAUDE_CODE_REQUIRED_SECRET_ENV);
assert.deepEqual(provider.provider_preflight['claude-code'].optional_secret_env, CLAUDE_CODE_OPTIONAL_SECRET_ENV);
assert.equal(provider.redacted_metadata_keys.includes('claude_code_auth'), true);
assert.equal(provider.capabilities.includes('repo_workspace'), true);
assert.equal(provider.capabilities.includes('patch_artifacts'), true);
assert.equal(provider.capabilities.includes('browser_runtime'), false);

const fixtureRuntimeTool = {
	schema: 'homeboy/resolved-agent-task-runtime-tool/v1',
	id: 'fixture.mcp', transport: 'stdio', argv: [process.execPath, '--fixture-mcp'],
	executable: process.execPath, env: { FIXTURE_MODE: 'isolated' }, secret_env_names: ['FIXTURE_MCP_TOKEN'], readiness: { status: 'ready', evidence: { kind: 'version_command', success: true } }, lifecycle: 'runtime_owned',
};

const manifest = JSON.parse(fs.readFileSync(path.join(runtimeRoot, 'claude-code.json'), 'utf8'));
assert.equal(manifest.id, 'claude-code');
assert.equal(manifest.name, 'Claude Code');
assert.equal(manifest.agent_task_executors.length, 1);
assert.equal(manifest.agent_task_executors[0].capabilities.includes('nested_orchestrator'), true);

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-claude-code-provider-contract-'));
try {
	const runtimesRoot = path.join(root, 'agent-runtimes');
	const runtimePath = path.join(runtimesRoot, 'claude-code');
	fs.mkdirSync(runtimesRoot, { recursive: true });
	fs.symlinkSync(runtimeRoot, runtimePath, 'dir');

	const [program, scriptTemplate] = provider.invocation.argv;
	assert.equal(program, 'node');
	const scriptPath = scriptTemplate.replaceAll('{{runtime_path}}', runtimePath);
	assert.equal(
		path.normalize(scriptPath),
		path.join(runtimePath, 'scripts', 'agent', 'homeboy-claude-code-agent-task-executor.cjs')
	);
	assert.equal(fs.existsSync(scriptPath), true, `provider command target should exist: ${scriptPath}`);

	const mockAdapterPath = path.join(root, 'mock-claude-code-adapter.cjs');
	fs.writeFileSync(mockAdapterPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
let raw = '';
process.stdin.on('data', (chunk) => { raw += chunk; });
process.stdin.on('end', () => {
  const request = JSON.parse(raw);
  assert.equal(request.executor.backend, 'claude-code');
  assert.equal(request.executor.runtime, 'claude-code');
   assert.equal(request.instructions, 'Prove the Claude Code provider boundary without leaking secrets.');
   assert.deepEqual(request.resolved_runtime_tools[0].argv, ${JSON.stringify(fixtureRuntimeTool.argv)});
   assert.equal(request.resolved_runtime_tools[0].env.FIXTURE_MODE, 'isolated');
   assert.equal(request.resolved_runtime_tools[0].env.FIXTURE_MCP_TOKEN, 'fixture-token-must-not-leak');
  assert.equal(process.env.UNDECLARED_SECRET, undefined);
  process.stdout.write(process.env.AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN || 'missing secret');
  process.stderr.write(process.env.AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN || 'missing secret');
  process.exit(0);
});
`);

	const contractResult = spawnSync(process.execPath, [scriptPath, '--provider-contract'], { encoding: 'utf8' });
	assert.equal(contractResult.status, 0, contractResult.stderr);
	assert.deepEqual(JSON.parse(contractResult.stdout), manifest.agent_task_executors[0]);

	const runRequest = {
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'claude-code-real-executor',
			executor: {
			backend: 'claude-code',
			runtime: 'claude-code',
			config: {
				provider: 'claude-code',
				command: process.execPath,
				command_args: [mockAdapterPath],
				artifacts_path: path.join(root, 'artifacts'),
			},
		},
		instructions: 'Prove the Claude Code provider boundary without leaking secrets.',
	};
	const runResult = spawnSync(process.execPath, [scriptPath], {
		encoding: 'utf8',
		env: {
			...process.env,
			AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN: 'refresh-token-must-not-leak',
			AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN: 'access-token-must-not-leak',
			AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT: 'expires-at-must-not-leak',
			FIXTURE_MCP_TOKEN: 'fixture-token-must-not-leak',
			UNDECLARED_SECRET: 'must-not-reach-claude-code',
		},
		input: JSON.stringify({ ...runRequest, resolved_runtime_tools: [fixtureRuntimeTool] }),
	});
	assert.equal(runResult.status, 0, runResult.stderr);
	const previousRefreshToken = process.env.AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN;
	const previousAccessToken = process.env.AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN;
	process.env.AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN = 'refresh-token-must-not-leak';
	process.env.AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN = 'access-token-must-not-leak';
	try {
		const parsedRun = JSON.parse(runResult.stdout);
		assert.equal(parsedRun.status, 'succeeded');
		assert.equal(parsedRun.artifacts.some((artifact) => artifact.stream === 'stdout'), true);
		assert.equal(parsedRun.artifacts.some((artifact) => artifact.stream === 'stderr'), true);
		for (const artifact of parsedRun.artifacts) {
			const content = fs.readFileSync(artifact.path, 'utf8');
			assert.equal(content.includes('refresh-token-must-not-leak'), false);
			assert.equal(content.includes('access-token-must-not-leak'), false);
			assert.equal(content.includes('expires-at-must-not-leak'), false);
		}
	} finally {
		if (previousRefreshToken === undefined) {
			delete process.env.AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN;
		} else {
			process.env.AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN = previousRefreshToken;
		}
		if (previousAccessToken === undefined) {
			delete process.env.AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN;
		} else {
			process.env.AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN = previousAccessToken;
		}
	}
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('refresh-token-must-not-leak'), false);
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('access-token-must-not-leak'), false);
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('expires-at-must-not-leak'), false);
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('fixture-token-must-not-leak'), false);
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('Claude Code agent task executor boundary passed\n');
