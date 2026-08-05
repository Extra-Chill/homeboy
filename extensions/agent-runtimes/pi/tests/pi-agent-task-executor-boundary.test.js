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
	executePiAgentTask,
	providerContract,
} = require('..');

const runtimeRoot = path.join(__dirname, '..');
const fixtureRuntimeTool = {
	schema: 'homeboy/resolved-agent-task-runtime-tool/v1',
	id: 'fixture.mcp', transport: 'stdio', argv: [process.execPath, '--fixture-mcp'],
	executable: process.execPath, env: { FIXTURE_MODE: 'isolated' }, readiness: { status: 'ready', evidence: { kind: 'version_command', success: true } }, lifecycle: 'runtime_owned',
};

const provider = providerContract();
assert.equal(provider.id, 'pi.agent-task-executor');
assert.equal(provider.backend, 'pi');
assert.equal(provider.runtime, 'pi');
assert.equal(provider.status, 'experimental');
assert.equal(provider.integration_contract, 'homeboy-pi-agent-task/v1');
assert.equal(Object.hasOwn(provider.lifecycle, 'max_concurrency_default'), false);
assert.equal(provider.lifecycle.cancellation, 'process_signal');
assert.deepEqual(provider.secret_env_requirements, []);
assert.deepEqual(provider.provider_defaults, {});
assert.equal(provider.capabilities.includes('cli_runtime'), true);
assert.equal(provider.capabilities.includes('structured_outcome'), true);
assert.equal(provider.capabilities.includes('repo_workspace'), false);

const manifest = JSON.parse(fs.readFileSync(path.join(runtimeRoot, 'pi.json'), 'utf8'));
assert.equal(manifest.id, 'pi');
assert.equal(manifest.name, 'Pi');
assert.equal(manifest.agent_task_executors.length, 1);

const validRequest = {
	schema: 'homeboy/agent-task-request/v1',
	task_id: 'pi-boundary',
	executor: {
		backend: 'pi',
		runtime: 'pi',
		config: {},
	},
	instructions: 'Validate the Pi provider boundary.',
};

const noCommand = executePiAgentTask(validRequest);
assert.equal(noCommand.status, 'no_op');
assert.equal(noCommand.metadata.configured, false);
assert.match(noCommand.diagnostics[0].message, /executor\.config\.command|HOMEBOY_PI_COMMAND/);

const invalid = executePiAgentTask({ ...validRequest, schema: 'wrong' });
assert.equal(invalid.status, 'provider_error');
assert.equal(invalid.failure_code, 'agent_task.invalid_pi_request');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-pi-provider-contract-'));
try {
	const runtimesRoot = path.join(root, 'agent-runtimes');
	const runtimePath = path.join(runtimesRoot, 'pi');
	fs.mkdirSync(runtimesRoot, { recursive: true });
	fs.symlinkSync(runtimeRoot, runtimePath, 'dir');

	const [program, scriptTemplate] = provider.invocation.argv;
	assert.equal(program, 'node');
	const scriptPath = scriptTemplate.replaceAll('{{runtime_path}}', runtimePath);
	assert.equal(
		path.normalize(scriptPath),
		path.join(runtimePath, 'scripts', 'agent', 'homeboy-pi-agent-task-executor.cjs')
	);
	assert.equal(fs.existsSync(scriptPath), true, `provider command target should exist: ${scriptPath}`);

	const contractResult = spawnSync(process.execPath, [scriptPath, '--provider-contract'], { encoding: 'utf8' });
	assert.equal(contractResult.status, 0, contractResult.stderr);
	assert.deepEqual(JSON.parse(contractResult.stdout), manifest.agent_task_executors[0]);

	const mockPiPath = path.join(root, 'mock-pi.cjs');
	fs.writeFileSync(mockPiPath, `#!/usr/bin/env node
const assert = require('node:assert/strict');
let raw = '';
process.stdin.on('data', (chunk) => { raw += chunk; });
process.stdin.on('end', () => {
  const stdinRequest = JSON.parse(raw);
  const envRequest = JSON.parse(process.env.HOMEBOY_AGENT_TASK_REQUEST);
  assert.equal(stdinRequest.task_id, 'pi-real-executor');
   assert.equal(envRequest.instructions, 'Validate the Pi provider boundary through a configured command.');
   assert.deepEqual(stdinRequest.resolved_runtime_tools[0].argv, ${JSON.stringify(fixtureRuntimeTool.argv)});
   assert.equal(envRequest.resolved_runtime_tools[0].env.FIXTURE_MODE, 'isolated');
  assert.equal(process.env.UNDECLARED_SECRET, undefined);
  process.exit(0);
});
`);

	const configuredRequest = {
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'pi-real-executor',
		executor: {
			backend: 'pi',
			runtime: 'pi',
			config: {
				command: process.execPath,
				command_args: [mockPiPath],
			},
		},
		instructions: 'Validate the Pi provider boundary through a configured command.',
	};
	const declaredWorkspace = path.join(root, 'declared-workspace');
	const declaredArtifacts = path.join(root, 'declared-artifacts');
	fs.mkdirSync(declaredWorkspace);
	fs.mkdirSync(declaredArtifacts);
	const declaredPiPath = path.join(root, 'mock-pi-declared.cjs');
	fs.writeFileSync(declaredPiPath, `#!/usr/bin/env node
const fs = require('node:fs');
fs.writeFileSync('pi-report.md', '# Pi report\\n');
process.stdin.resume();
process.stdin.on('end', () => process.exit(0));
`);
	const declaredResult = executePiAgentTask({
		...configuredRequest,
		task_id: 'pi-declared-artifact',
		workspace_path: declaredWorkspace,
		artifacts_path: declaredArtifacts,
		artifact_declarations: [{ name: 'report', path: 'pi-report.md', kind: 'markdown', required: true }],
		executor: { backend: 'pi', runtime: 'pi', config: { command: process.execPath, command_args: [declaredPiPath], cwd: declaredWorkspace } },
	});
	assert.equal(declaredResult.artifacts.some((artifact) => artifact.name === 'report'), true);
	assert.equal(declaredResult.evidence_refs.some((ref) => ref.label === 'report'), true);
	const runResult = spawnSync(process.execPath, [scriptPath], {
		encoding: 'utf8',
		env: { ...process.env, UNDECLARED_SECRET: 'must-not-reach-pi' },
		input: JSON.stringify({ ...configuredRequest, resolved_runtime_tools: [fixtureRuntimeTool] }),
	});
	assert.equal(runResult.status, 0, runResult.stderr);
	assert.deepEqual(JSON.parse(runResult.stdout), executePiAgentTask({ ...configuredRequest, resolved_runtime_tools: [fixtureRuntimeTool] }));
	assert.equal(JSON.parse(runResult.stdout).status, 'no_op');
	assert.equal(JSON.parse(runResult.stdout).metadata.exit_code, 0);

	const failedCommand = executePiAgentTask({
		...configuredRequest,
		executor: {
			backend: 'pi',
			runtime: 'pi',
			config: {
				command: process.execPath,
				command_args: ['-e', 'process.exit(2)'],
			},
		},
	});
	assert.equal(failedCommand.status, 'provider_error');
	assert.equal(failedCommand.failure_code, 'agent_task.pi_command_failed');
	assert.equal(failedCommand.metadata.exit_code, 2);
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('Pi agent task executor boundary passed\n');
