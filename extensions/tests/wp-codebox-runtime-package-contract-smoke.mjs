#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(rootDir, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');

const {
	codeboxTaskRequestFromAgentTaskRequest,
	normalizeProviderPluginPaths,
	normalizeRuntimePackageTaskPackage,
	runtimePackageTaskInputForCodebox,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'codebox-agent-task-executor.js'));

const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'hbe-codebox-runtime-package-'));

try {
	const providerPath = path.join(tempRoot, 'ai-provider-for-openai');
	const staleProviderPath = path.join(tempRoot, 'stale-ai-provider-for-openai');
	const workspaceRoot = path.join(tempRoot, 'workspace');
	fs.mkdirSync(providerPath, { recursive: true });
	fs.mkdirSync(staleProviderPath, { recursive: true });
	fs.mkdirSync(workspaceRoot, { recursive: true });

	assert.deepEqual(normalizeProviderPluginPaths([
		providerPath,
		{ path: providerPath },
		{ source: staleProviderPath },
	]), [providerPath, staleProviderPath]);

	const taskInput = codeboxTaskRequestFromAgentTaskRequest({
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'runtime-package-contract-smoke',
		instructions: 'Run a runtime package with declared artifacts.',
		executor: {
			backend: 'wp-codebox',
			model: 'gpt-5.5',
			config: {
				provider: 'codex',
				provider_plugin_paths: [providerPath],
				runtime_requirements: {
					provider_plugins: [{ path: staleProviderPath }],
				},
				runtime_task: {
					ability: 'wp-codebox/run-runtime-package',
					input: {
						runtime_package: {
							slug: 'proof-loop',
							source: 'packages/proof-loop',
						},
					},
				},
			},
		},
		inputs: { target: { root: workspaceRoot } },
		artifact_declarations: [{
			schema: 'homeboy/agent-task-artifact-declaration/v1',
			name: 'review_packet',
			artifact_schema: 'example/review-packet/v1',
			required: true,
		}],
	}, {
		settings: {
			provider_plugin_paths: { codex: [staleProviderPath] },
		},
	});

	assert.deepEqual(taskInput.provider_plugin_paths, [providerPath]);
	assert.deepEqual(taskInput.runtime_requirements.provider_plugins, [{ path: providerPath }]);

	const runtimeTask = taskInput.runtime_task;
	assert.equal(runtimeTask.ability, 'wp-codebox/run-runtime-package');
	assert.equal(runtimeTask.input.schema, 'wp-codebox/runtime-package-task/v1');
	assert.deepEqual(runtimeTask.input.package, {
		slug: 'proof-loop',
		source: '/workspace/workspace/packages/proof-loop',
	});
	assert.deepEqual(runtimeTask.input.artifact_declarations, [{
		schema: 'wp-codebox/artifact-declaration/v1',
		name: 'review_packet',
		artifact_schema: 'example/review-packet/v1',
		required: true,
	}]);
	assert.deepEqual(runtimeTask.input.required_artifacts, ['review_packet']);
	assert.equal(Object.hasOwn(runtimeTask.input, 'runtime_package'), false);
	assert.equal(Object.hasOwn(runtimeTask.input, 'agent'), false);
	assert.equal(Object.hasOwn(runtimeTask.input, 'engine_data_outputs'), false);
	assert.equal(Object.hasOwn(runtimeTask.input, 'artifacts'), false);

	assert.deepEqual(runtimePackageTaskInputForCodebox({
		package: { slug: 'proof-loop', source: 'packages/proof-loop' },
	}, { workspaceTarget: '/workspace/source' }), {
		schema: 'wp-codebox/runtime-package-task/v1',
		package: {
			slug: 'proof-loop',
			source: '/workspace/source/packages/proof-loop',
		},
		artifact_declarations: [],
	});

	assert.throws(
		() => normalizeRuntimePackageTaskPackage({ source: 'packages/one', path: 'packages/two' }, { workspaceTarget: '/workspace/source' }),
		/runtime_package descriptor source fields cannot diverge/
	);

	console.log('wp-codebox runtime package contract smoke passed');
} finally {
	fs.rmSync(tempRoot, { recursive: true, force: true });
}
