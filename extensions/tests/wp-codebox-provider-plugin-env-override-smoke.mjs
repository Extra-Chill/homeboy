#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(rootDir, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');

const { codeboxTaskRequestFromAgentTaskRequest } = require(
	path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'codebox-agent-task-executor.js')
);

const taskRunner = path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs');
const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-provider-env-'));
const staleProviderPath = path.join(tempRoot, 'ai-provider-for-openai@codex-oauth-provider');
const explicitProviderPath = path.join(tempRoot, 'ai-provider-for-openai@proof-codex-provider-20260627');
const workspaceRoot = path.join(tempRoot, 'workspace');
const artifactsPath = path.join(tempRoot, 'artifacts');
const capturePath = path.join(tempRoot, 'capture.json');
const fakeWpCodebox = path.join(tempRoot, 'fake-wp-codebox.cjs');

fs.mkdirSync(staleProviderPath, { recursive: true });
fs.mkdirSync(explicitProviderPath, { recursive: true });
fs.mkdirSync(workspaceRoot, { recursive: true });
fs.writeFileSync(path.join(explicitProviderPath, 'composer.json'), JSON.stringify({ name: 'extra-chill/ai-provider-for-openai' }));
fs.writeFileSync(path.join(explicitProviderPath, 'plugin.php'), '<?php\n/* Plugin Name: AI Provider for OpenAI Codex */\n// Registers the codex provider.\n');
fs.writeFileSync(fakeWpCodebox, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
if (process.argv[2] === 'run-agent-task' && process.argv[3] === '--help') {
  process.exit(0);
}
const inputArg = process.argv.find((arg) => arg.startsWith('--input-file='));
const inputPath = inputArg ? inputArg.slice('--input-file='.length) : '';
const envelope = JSON.parse(fs.readFileSync(inputPath, 'utf8'));
fs.writeFileSync(${JSON.stringify(capturePath)}, JSON.stringify(envelope.task_input || envelope.input || envelope, null, 2));
process.stdout.write(JSON.stringify({ schema: 'wp-codebox/agent-task-run/v1', success: true, status: 'completed', artifact_result: { schema: 'wp-codebox/artifact-result-envelope/v1', status: 'created' } }));
`);
fs.chmodSync(fakeWpCodebox, 0o755);

const previousSettings = process.env.HOMEBOY_SETTINGS_JSON;
const previousProviderPaths = process.env.HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATHS;
try {
	process.env.HOMEBOY_SETTINGS_JSON = JSON.stringify({
		provider_plugin_paths: { codex: [staleProviderPath] },
	});
	process.env.HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATHS = explicitProviderPath;

	const taskInput = codeboxTaskRequestFromAgentTaskRequest({
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'provider-plugin-env-override',
		executor: {
			backend: 'codebox',
			model: 'gpt-5.5',
			config: {
				provider: 'codex',
				runtime_requirements: {
					provider_plugins: [{ path: staleProviderPath }],
				},
			},
		},
		instructions: 'Verify generic provider plugin env override.',
		inputs: { target: { root: workspaceRoot } },
	});

	assert.deepEqual(taskInput.provider_plugin_paths, [explicitProviderPath]);
	assert.deepEqual(taskInput.runtime_requirements.provider_plugins, [{ path: explicitProviderPath }]);
	assert.equal(JSON.stringify(taskInput.runtime_requirements).includes(staleProviderPath), false);

	const result = spawnSync(process.execPath, [
		taskRunner,
		'--wp-codebox-bin', fakeWpCodebox,
		'--artifacts', artifactsPath,
	], {
		encoding: 'utf8',
		input: JSON.stringify({
			...taskInput,
			provider_plugin_paths: [staleProviderPath],
		}),
		env: {
			...process.env,
			HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATHS: explicitProviderPath,
			AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'test-access-token',
			AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'test-refresh-token',
			AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '1893456000',
			AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'test-account-id',
			AI_PROVIDER_OPENAI_CODEX_FEDRAMP: 'false',
		},
	});

	assert.equal(result.status, 0, result.stderr || result.stdout);
	const captured = JSON.parse(fs.readFileSync(capturePath, 'utf8'));
	assert.deepEqual(captured.provider_plugin_paths, [explicitProviderPath]);
	assert.equal(captured.extra_plugins.some((plugin) => plugin.source.includes('codex-oauth-provider')), false);
	const providerPlugin = captured.extra_plugins.find((plugin) => plugin.source === explicitProviderPath);
	assert(providerPlugin);
	assert.equal(providerPlugin.slug, 'ai-provider-for-openai');
	assert.equal(providerPlugin.pluginFile, 'ai-provider-for-openai/plugin.php');
	assert.equal(captured.extra_plugins.some((plugin) => String(plugin.pluginFile || '').includes('ai-provider-for-openai-proof-codex-provider-20260627.php')), false);
	assert.equal(captured.extra_plugins.some((plugin) => String(plugin.pluginFile || '') === 'ai-provider-for-openai/ai-provider-for-openai.php'), false);

	console.log('wp-codebox provider plugin env override smoke passed');
} finally {
	if (previousSettings === undefined) {
		delete process.env.HOMEBOY_SETTINGS_JSON;
	} else {
		process.env.HOMEBOY_SETTINGS_JSON = previousSettings;
	}
	if (previousProviderPaths === undefined) {
		delete process.env.HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATHS;
	} else {
		process.env.HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATHS = previousProviderPaths;
	}
	fs.rmSync(tempRoot, { recursive: true, force: true });
}
