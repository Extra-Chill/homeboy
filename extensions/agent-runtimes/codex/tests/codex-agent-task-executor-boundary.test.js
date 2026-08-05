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
assert.equal(Object.hasOwn(provider.provider_defaults.codex, 'model'), false);
assert.equal(provider.provider_defaults.codex.command, CODEX_DEFAULT_COMMAND);
assert.deepEqual(provider.provider_defaults.codex.command_args, CODEX_DEFAULT_COMMAND_ARGS);
assert.equal(provider.redacted_metadata_keys.includes('codex_auth'), true);
assert.equal(provider.capabilities.includes('repo_workspace'), true);
assert.equal(provider.capabilities.includes('patch_artifacts'), true);
assert.equal(provider.capabilities.includes('nested_orchestrator'), false);
assert.equal(provider.capabilities.includes('wordpress_sandbox'), false);

const fixtureRuntimeTool = {
	schema: 'homeboy/resolved-agent-task-runtime-tool/v1',
	id: 'fixture.mcp',
	transport: 'stdio',
	argv: [process.execPath, '--fixture-mcp', '--isolated'],
	executable: process.execPath,
	env: { FIXTURE_MODE: 'isolated' },
	secret_env_names: ['FIXTURE_MCP_TOKEN'],
	readiness: { status: 'ready', evidence: { kind: 'version_command', success: true } },
	lifecycle: 'runtime_owned',
};

const manifest = JSON.parse(fs.readFileSync(path.join(runtimeRoot, 'codex.json'), 'utf8'));
assert.equal(manifest.id, 'codex');
assert.equal(manifest.name, 'Codex');
assert.equal(manifest.agent_task_executors.length, 1);
assert.equal(Object.hasOwn(manifest.agent_task_executors[0].provider_defaults.codex, 'model'), false);

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
	const declaredArtifactCliPath = path.join(root, 'mock-codex-declared-artifacts.cjs');
	const omittedModelCliPath = path.join(root, 'mock-codex-without-model.cjs');
	fs.writeFileSync(omittedModelCliPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
assert.deepEqual(process.argv.slice(2), ['exec', 'Run without selecting a model.']);
process.exit(0);
`);
	fs.writeFileSync(mockCliPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
assert.equal(process.argv.includes('exec'), true);
assert.equal(process.argv.includes('--model'), true);
assert.equal(process.argv.includes('gpt-5.5'), true);
assert.equal(process.argv.at(-1), 'Prove the Codex runtime boundary without leaking secrets.');
assert.equal(process.env.UNDECLARED_SECRET, undefined);
process.stdout.write(process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN || 'missing secret');
process.stderr.write(process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN || 'missing secret');
process.exit(0);
`);
	fs.writeFileSync(declaredArtifactCliPath, `#!/usr/bin/env node
const fs = require('node:fs');
fs.mkdirSync('screenshots', { recursive: true });
fs.writeFileSync('report.md', '# Captured report\\n');
fs.writeFileSync('screenshots/image.bin', Buffer.from([0, 255, 1]));
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
	const omittedModelResult = executeCodexAgentTask({
		...request,
		task_id: 'codex-without-model',
		executor: {
			...request.executor,
			config: {
				provider: 'codex',
				command: process.execPath,
				command_args: [omittedModelCliPath, 'exec'],
				artifacts_path: path.join(root, 'omitted-model-artifacts'),
			},
		},
		instructions: 'Run without selecting a model.',
	});
	assert.equal(omittedModelResult.status, 'succeeded', JSON.stringify(omittedModelResult.diagnostics));
	const declaredWorkspace = path.join(root, 'declared-workspace');
	const declaredArtifactRoot = path.join(root, 'declared-artifacts');
	fs.mkdirSync(declaredWorkspace);
	fs.mkdirSync(declaredArtifactRoot);
	const declaredResult = executeCodexAgentTask({
		...request,
		task_id: 'codex-declared-artifacts',
		executor: {
			...request.executor,
			config: {
				provider: 'codex',
				command: process.execPath,
				command_args: [declaredArtifactCliPath, 'exec'],
				cwd: declaredWorkspace,
				artifacts_path: declaredArtifactRoot,
			},
		},
		artifact_declarations: [
			{ name: 'report', path: 'report.md', kind: 'markdown', required: true },
			{ name: 'screenshots', path: 'screenshots', kind: 'screenshot-directory', required: true },
		],
	});
	assert.equal(declaredResult.status, 'succeeded', JSON.stringify(declaredResult.diagnostics));
	const declaredReport = declaredResult.artifacts.find((artifact) => artifact.name === 'report');
	const declaredScreenshots = declaredResult.artifacts.find((artifact) => artifact.name === 'screenshots');
	assert.equal(fs.readFileSync(declaredReport.path, 'utf8'), '# Captured report\n');
	assert.deepEqual(fs.readFileSync(path.join(declaredScreenshots.path, 'image.bin')), Buffer.from([0, 255, 1]));
	assert.equal(declaredScreenshots.file_count, 1);
	const missingDeclaredArtifactRoot = path.join(root, 'missing-declared-artifacts');
	fs.mkdirSync(missingDeclaredArtifactRoot);
	const missingDeclaredResult = executeCodexAgentTask({
		...request,
		task_id: 'codex-missing-declared-artifact',
		executor: {
			...request.executor,
			config: {
				provider: 'codex',
				command: process.execPath,
				command_args: [omittedModelCliPath, 'exec'],
				cwd: declaredWorkspace,
				artifacts_path: missingDeclaredArtifactRoot,
			},
		},
		instructions: 'Run without selecting a model.',
		artifact_declarations: [{ name: 'required-report', path: 'missing.md', required: true }],
	});
	assert.equal(missingDeclaredResult.status, 'failed');
	assert.equal(missingDeclaredResult.failure_code, 'agent_task.declared_artifact_harvest_failed');
	assert.equal(missingDeclaredResult.diagnostics.some((diagnostic) => diagnostic.class === 'agent_task.required_declared_artifact_missing'), true);
	const runResult = spawnSync(process.execPath, [scriptPath], {
		encoding: 'utf8',
		env: {
			...process.env,
			AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'refresh-token-must-not-leak',
			AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'access-token-must-not-leak',
			FIXTURE_MCP_TOKEN: 'fixture-token-must-not-leak',
			UNDECLARED_SECRET: 'must-not-reach-codex',
		},
		input: JSON.stringify({ ...request, resolved_runtime_tools: [fixtureRuntimeTool] }),
	});
	assert.equal(runResult.status, 0, runResult.stderr);
	const parsedRun = JSON.parse(runResult.stdout);
	const previousRefreshToken = process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN;
	const previousAccessToken = process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN;
	process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN = 'refresh-token-must-not-leak';
	process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN = 'access-token-must-not-leak';
	try {
		assert.equal(parsedRun.status, 'succeeded');
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
	assert.equal(`${runResult.stdout}\n${runResult.stderr}`.includes('fixture-token-must-not-leak'), false);
	assert.equal(executeCodexAgentTask({ ...request, executor: { backend: 'codex', runtime: 'wrong', config: {} } }).failure_code, 'agent_task.invalid_codex_request');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('Codex agent task executor boundary passed\n');
