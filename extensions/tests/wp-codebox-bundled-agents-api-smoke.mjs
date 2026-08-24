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
	const agentsApi = path.join(fixtureRoot, 'agents-api');
	const artifacts = path.join(fixtureRoot, 'artifacts');
	const capturePath = path.join(fixtureRoot, 'captured-input.json');
	const fakeCodeboxBin = path.join(fixtureRoot, 'wp-codebox-fake.mjs');

	mkdirSync(agentsApi, { recursive: true });
	mkdirSync(artifacts, { recursive: true });
	writeFileSync(path.join(agentsApi, 'agents-api.php'), "<?php\n/* Plugin Name: Agents API */\n");
	writeFileSync(fakeCodeboxBin, `#!/usr/bin/env node
import { readFileSync, writeFileSync } from 'node:fs';
if (process.argv.includes('--version')) {
  process.stdout.write('0.21.0');
  process.exit(0);
}
if (process.argv[2] === 'runtime' && process.argv[3] === 'descriptor' && process.argv.includes('--json')) {
  process.stdout.write(JSON.stringify({
    schema: 'wp-codebox/runtime-descriptor/v1',
    readiness: { status: 'available', browserRuntime: { status: 'ready' } },
    contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } },
    commands: ['run-agent-task'],
  }));
  process.exit(0);
}
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
			agents_api: agentsApi,
		},
	};

	const result = spawnSync(process.execPath, [runner], {
		input: JSON.stringify(request),
		encoding: 'utf8',
		env: {
			...process.env,
			HOMEBOY_WP_CODEBOX_CORE_MODULE: path.join(rootDir, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs'),
			HOMEBOY_CAPTURE_TASK_INPUT: capturePath,
		},
	});

	assert.equal(result.status, 0, result.stderr || result.stdout);

	const capturedEnvelope = JSON.parse(readFileSync(capturePath, 'utf8'));
	const captured = capturedEnvelope.task_input || capturedEnvelope;
	const agentsApiContract = (captured.component_contracts || []).find((contract) => contract.slug === 'agents-api');
	const agentsApiPlugin = (captured.extra_plugins || []).find((plugin) => plugin.slug === 'agents-api');

	assert.equal(agentsApiContract, undefined, 'agents-api component contract is not inferred from runtime component paths');
	assert.equal(agentsApiPlugin, undefined, 'agents-api extra plugin is not inferred from runtime component paths');

	const runtimeRequirementsCapturePath = path.join(fixtureRoot, 'captured-runtime-requirements-input.json');
	const runtimeRequirementsRequest = {
		...request,
		runtime_component_paths: {},
		component_contracts: [],
		runtime_requirements: {
			component_contracts: [
				{ slug: 'agents-api', path: agentsApi, loadAs: 'mu-plugin', activate: false },
			],
			extra_plugins: [
				{ slug: 'agents-api', source: agentsApi, loadAs: 'mu-plugin', activate: false },
			],
		},
	};
	const runtimeRequirementsResult = spawnSync(process.execPath, [runner], {
		input: JSON.stringify(runtimeRequirementsRequest),
		encoding: 'utf8',
		env: {
			...process.env,
			HOMEBOY_WP_CODEBOX_CORE_MODULE: path.join(rootDir, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs'),
			HOMEBOY_CAPTURE_TASK_INPUT: runtimeRequirementsCapturePath,
		},
	});
	assert.equal(runtimeRequirementsResult.status, 0, runtimeRequirementsResult.stderr || runtimeRequirementsResult.stdout);
	const runtimeRequirementsEnvelope = JSON.parse(readFileSync(runtimeRequirementsCapturePath, 'utf8'));
	const runtimeRequirementsCaptured = runtimeRequirementsEnvelope.task_input || runtimeRequirementsEnvelope;
	assert.ok(
		runtimeRequirementsCaptured.component_contracts.some((contract) => contract.slug === 'agents-api'),
		'agents-api component contract is preserved from runtime_requirements when top-level contracts are empty'
	);
	assert.equal(
		(runtimeRequirementsCaptured.extra_plugins || []).some((plugin) => plugin.slug === 'agents-api'),
		false,
		'agents-api extra plugin is not inferred by the generic task runner'
	);

	console.log('wp-codebox agents-api boundary smoke passed');
} finally {
	rmSync(fixtureRoot, { recursive: true, force: true });
}
