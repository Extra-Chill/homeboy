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
			backend: 'codebox',
			model: 'gpt-5.5',
			config: {
				provider: 'codex',
				provider_plugin_paths: [providerPath],
				runtime_requirements: {
					provider_plugins: [{ path: staleProviderPath }],
				},
				runtime_task: {
					ability: 'homeboy/run-runtime-package',
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
	assert.equal(runtimeTask.input.runtime_package, '/workspace/workspace/packages/proof-loop');
	assert.equal(runtimeTask.input.metadata.runtime_package_descriptor.source, runtimeTask.input.runtime_package);
	assert.equal(runtimeTask.input.required_artifacts.includes('review_packet'), true);
	assert.deepEqual(runtimeTask.input.artifacts, [{
		name: 'review_packet',
		required: true,
		schema: 'example/review-packet/v1',
		output: 'outputs.typed_artifacts.review_packet.payload',
	}]);
	assert.equal(runtimeTask.input.engine_data_outputs.review_packet, 'outputs.typed_artifacts.review_packet.payload');

	assert.deepEqual(runtimePackageTaskInputForCodebox({
		package: { slug: 'proof-loop', source: 'packages/proof-loop' },
	}, { workspaceTarget: '/workspace/source' }), {
		runtime_package: '/workspace/source/packages/proof-loop',
		agent: 'proof-loop',
		metadata: {
			runtime_package_descriptor: {
				slug: 'proof-loop',
				source: '/workspace/source/packages/proof-loop',
			},
		},
	});

	assert.throws(
		() => normalizeRuntimePackageTaskPackage({ source: 'packages/one', path: 'packages/two' }, { workspaceTarget: '/workspace/source' }),
		/runtime_package descriptor source fields cannot diverge/
	);

	console.log('wp-codebox runtime package contract smoke passed');
} finally {
	fs.rmSync(tempRoot, { recursive: true, force: true });
}
