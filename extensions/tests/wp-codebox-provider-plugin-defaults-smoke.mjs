#!/usr/bin/env node
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(rootDir, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const { codeboxTaskRequestFromAgentTaskRequest } = require(
	path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'codebox-agent-task-executor.js')
);
const {
	WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA,
	assertProviderCredentialBoundaryNamesOnly,
	providerCredentialSecretEnvNames,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox'));

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
	assert.equal(taskInput.provider_credential_boundary.schema, WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA);

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
	assert.deepEqual(providerCredentialSecretEnvNames({ secret_env: ['OPENAI_API_KEY'] }, { recipe: { secret_env: ['GITHUB_TOKEN'] } }), [
		'OPENAI_API_KEY',
		'GITHUB_TOKEN',
	]);
	assert.throws(
		() => assertProviderCredentialBoundaryNamesOnly({ secret_env_values: { OPENAI_API_KEY: 'sk-secret' } }),
		/secret_env names only/
	);
	console.log('wp-codebox provider plugin defaults smoke passed');
} finally {
	rmSync(providerDir, { recursive: true, force: true });
	rmSync(explicitProviderDir, { recursive: true, force: true });
}
