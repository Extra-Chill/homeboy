#!/usr/bin/env node
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { codeboxTaskRequestFromAgentTaskRequest } = require(
	path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'codebox-agent-task-executor.js')
);

const providerDir = mkdtempSync(path.join(tmpdir(), 'homeboy-wp-codebox-provider-'));
const explicitProviderDir = mkdtempSync(path.join(tmpdir(), 'homeboy-wp-codebox-explicit-provider-'));

try {
	const taskRequest = {
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'wp-codebox-provider-plugin-defaults-smoke',
		instructions: 'Validate provider plugin path defaults.',
		executor: {
			backend: 'codebox',
			config: {
				provider: 'codex',
				model: 'gpt-5.5',
				provider_plugin_paths: [],
			},
		},
	};
	const options = {
		settings: {
			provider_plugin_paths: [providerDir],
		},
	};

	const taskInput = codeboxTaskRequestFromAgentTaskRequest(taskRequest, options);

	assert.deepEqual(taskInput.provider_plugin_paths, [providerDir]);

	const explicitTaskInput = codeboxTaskRequestFromAgentTaskRequest({
		...taskRequest,
		executor: {
			...taskRequest.executor,
			config: {
				...taskRequest.executor.config,
				provider_plugin_paths: [explicitProviderDir],
			},
		},
	}, options);

	assert.deepEqual(explicitTaskInput.provider_plugin_paths, [explicitProviderDir]);
	console.log('wp-codebox provider plugin defaults smoke passed');
} finally {
	rmSync(providerDir, { recursive: true, force: true });
	rmSync(explicitProviderDir, { recursive: true, force: true });
}
