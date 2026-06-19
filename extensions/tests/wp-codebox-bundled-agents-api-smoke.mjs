#!/usr/bin/env node
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const runner = path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs');
const fixtureRoot = mkdtempSync(path.join(tmpdir(), 'homeboy-codebox-agents-api-'));

try {
	const dataMachine = path.join(fixtureRoot, 'data-machine');
	const agentsApi = path.join(dataMachine, 'vendor', 'wordpress', 'agents-api');
	const dataMachineCode = path.join(fixtureRoot, 'data-machine-code');
	const sandboxDataMachine = '/sandbox/runtime/data-machine';
	const artifacts = path.join(fixtureRoot, 'artifacts');
	const capturePath = path.join(fixtureRoot, 'captured-input.json');
	const fakeCodeboxBin = path.join(fixtureRoot, 'wp-codebox-fake.mjs');

	mkdirSync(agentsApi, { recursive: true });
	mkdirSync(dataMachineCode, { recursive: true });
	mkdirSync(artifacts, { recursive: true });
	writeFileSync(path.join(dataMachine, 'data-machine.php'), "<?php\n/* Plugin Name: Data Machine */\n");
	writeFileSync(path.join(dataMachineCode, 'data-machine-code.php'), "<?php\n/* Plugin Name: Data Machine Code */\n");
	writeFileSync(path.join(agentsApi, 'agents-api.php'), "<?php\n/* Plugin Name: Agents API */\n");
	writeFileSync(fakeCodeboxBin, `#!/usr/bin/env node
import { readFileSync, writeFileSync } from 'node:fs';
const inputArg = process.argv.find((arg) => arg.startsWith('--input-file='));
const inputPath = inputArg ? inputArg.slice('--input-file='.length) : '';
const input = JSON.parse(readFileSync(inputPath, 'utf8'));
writeFileSync(process.env.HOMEBOY_CAPTURE_TASK_INPUT, JSON.stringify(input, null, 2));
process.stdout.write(JSON.stringify({
  schema: 'wp-codebox/agent-task-run/v1',
  success: true,
  status: 'completed',
  task: input.goal,
  run: { success: true },
  agent_result: {},
  agent_task_result: {},
  completion_outcome: {},
  component_contracts: input.component_contracts || [],
  diagnostics: [],
  evidence_refs: []
}, null, 2) + '\\n');
`);
	chmodSync(fakeCodeboxBin, 0o755);

	const request = {
		schema: 'wp-codebox/task-input/v1',
		provider: 'fixture-provider',
		model: 'fixture-model',
		goal: 'Capture inferred runtime components.',
		wp_codebox_bin: fakeCodeboxBin,
		artifacts_path: artifacts,
		runtime_component_paths: {
			agent_runtime: sandboxDataMachine,
			agent_runtime_tools: dataMachineCode,
		},
		component_contracts: [
			{ slug: 'data-machine', path: dataMachine, loadAs: 'mu-plugin', activate: false },
		],
	};

	const result = spawnSync(process.execPath, [runner], {
		input: JSON.stringify(request),
		encoding: 'utf8',
		env: {
			...process.env,
			HOMEBOY_CAPTURE_TASK_INPUT: capturePath,
		},
	});

	assert.equal(result.status, 0, result.stderr || result.stdout);

	const captured = JSON.parse(readFileSync(capturePath, 'utf8'));
	const agentsApiContract = captured.component_contracts.find((contract) => contract.slug === 'agents-api');
	const agentsApiPlugin = captured.extra_plugins.find((plugin) => plugin.slug === 'agents-api');

	assert.ok(agentsApiContract, 'agents-api component contract is inferred from Data Machine vendor dependencies');
	assert.ok(agentsApiPlugin, 'agents-api extra plugin is emitted for WP Codebox recipe mounting');
	assert.equal(captured.runtime_component_paths.agent_runtime, sandboxDataMachine, 'sandbox runtime path is preserved for the mounted Data Machine component');
	assert.equal(agentsApiPlugin.loadAs, 'mu-plugin');
	assert.equal(agentsApiPlugin.activate, false);
	assert.ok(agentsApiPlugin.source.endsWith(`${path.sep}vendor${path.sep}wordpress${path.sep}agents-api`), `agents-api source points at the bundled Composer dependency: ${agentsApiPlugin.source}`);

	const runtimeRequirementsCapturePath = path.join(fixtureRoot, 'captured-runtime-requirements-input.json');
	const runtimeRequirementsRequest = {
		...request,
		runtime_component_paths: {},
		component_contracts: [],
		runtime_requirements: {
			component_contracts: [
				{ slug: 'agents-api', path: agentsApi, loadAs: 'mu-plugin', activate: false },
			],
		},
	};
	const runtimeRequirementsResult = spawnSync(process.execPath, [runner], {
		input: JSON.stringify(runtimeRequirementsRequest),
		encoding: 'utf8',
		env: {
			...process.env,
			HOMEBOY_CAPTURE_TASK_INPUT: runtimeRequirementsCapturePath,
		},
	});
	assert.equal(runtimeRequirementsResult.status, 0, runtimeRequirementsResult.stderr || runtimeRequirementsResult.stdout);
	const runtimeRequirementsCaptured = JSON.parse(readFileSync(runtimeRequirementsCapturePath, 'utf8'));
	assert.ok(
		runtimeRequirementsCaptured.component_contracts.some((contract) => contract.slug === 'agents-api'),
		'agents-api component contract is preserved from runtime_requirements when top-level contracts are empty'
	);
	assert.ok(
		runtimeRequirementsCaptured.extra_plugins.some((plugin) => plugin.slug === 'agents-api'),
		'agents-api extra plugin is emitted from runtime_requirements when top-level contracts are empty'
	);

	console.log('wp-codebox bundled agents-api smoke passed');
} finally {
	rmSync(fixtureRoot, { recursive: true, force: true });
}
