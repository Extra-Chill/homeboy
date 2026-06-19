#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { codeboxTaskRequestFromAgentTaskRequest } = require(
	path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'codebox-agent-task-executor.js')
);

const previousPolicy = process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON;

try {
	process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON = '{"schema":"homeboy/agent-tool-policy/v1","tools":{}}';

	const taskInput = codeboxTaskRequestFromAgentTaskRequest({
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'wp-codebox-runtime-profile-defaults-smoke',
		instructions: 'Validate profile-driven runtime defaults.',
		executor: {
			backend: 'codebox',
			config: {
				provider: 'codex',
				model: 'gpt-5.5',
				runtime_task: {
					ability: 'example/run-task',
					input: {},
				},
				runtime_profile: 'example-runtime',
				runtime_profiles: {
					'example-runtime': {
						schema: 'homeboy/runtime-profile/v1',
						id: 'example-runtime',
						runtime_task_ability: 'example/run-task',
						runtime_env_aliases: {
							HOMEBOY_AGENT_TOOL_POLICY_JSON: ['EXAMPLE_RUNTIME_TOOL_POLICY_JSON'],
						},
					},
				},
			},
		},
	});

	assert.equal(taskInput.runtime_env.EXAMPLE_RUNTIME_TOOL_POLICY_JSON, process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON);
	assert.equal(taskInput.runtime_env.DATAMACHINE_HOST_TOOL_POLICY_JSON, undefined);

	const genericProfileTaskInput = codeboxTaskRequestFromAgentTaskRequest({
		schema: 'homeboy/agent-task-request/v1',
		task_id: 'wp-codebox-generic-runtime-profile-smoke',
		instructions: 'Validate generic WP Codebox runtime profile primitives.',
		executor: {
			backend: 'codebox',
			config: {
				provider: 'codex',
				model: 'gpt-5.5',
				runtime_task: {
					ability: 'example/run-task',
					input: {},
				},
				runtime_profile: {
					schema: 'wp-codebox/runtime-profile/v1',
					id: 'generic-codebox-runtime',
					components: [
						{ kind: 'component', slug: 'agents-api', source: '/runtime/agents-api', pluginFile: 'agents-api/agents-api.php' },
						{ kind: 'parent-tool-bridge', slug: 'codebox-parent-tools', source: '/runtime/codebox-parent-tools' },
					],
					mu_plugins: [
						{ kind: 'mu_plugin', slug: 'runtime-tools', source: '/runtime/runtime-tools' },
					],
					plugins: [
						{ kind: 'plugin', slug: 'provider-plugin', source: '/runtime/provider-plugin', activate: true },
					],
					env: { EXAMPLE_RUNTIME: '1' },
					provider_plugins: [{ path: '/runtime/provider-plugin' }],
				},
			},
		},
	});

	assert.equal(genericProfileTaskInput.runtime_requirements.schema, 'wp-codebox/runtime-profile/v1');
	assert.equal(genericProfileTaskInput.runtime_requirements.id, 'generic-codebox-runtime');
	assert.deepEqual(genericProfileTaskInput.runtime_requirements.components.map((component) => component.slug), [
		'agents-api',
		'codebox-parent-tools',
	]);
	assert.deepEqual(genericProfileTaskInput.runtime_requirements.component_contracts.map((contract) => contract.slug), [
		'agents-api',
		'codebox-parent-tools',
		'runtime-tools',
		'provider-plugin',
	]);
	assert.deepEqual(genericProfileTaskInput.component_contracts.map((contract) => contract.slug), [
		'agents-api',
		'codebox-parent-tools',
		'runtime-tools',
		'provider-plugin',
	]);
	assert.equal(genericProfileTaskInput.runtime_requirements.component_contracts[0].loadAs, 'mu-plugin');
	assert.equal(genericProfileTaskInput.runtime_requirements.component_contracts[3].loadAs, 'plugin');
	assert.equal(genericProfileTaskInput.runtime_requirements.homeboy_parent_tool_bridge, undefined);
	assert.deepEqual(genericProfileTaskInput.runtime_requirements.provider_plugins, [{ path: '/runtime/provider-plugin' }]);
	assert.equal(genericProfileTaskInput.runtime_requirements.env.EXAMPLE_RUNTIME, '1');

	console.log('wp-codebox runtime profile defaults smoke passed');
} finally {
	if (previousPolicy === undefined) {
		delete process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON;
	} else {
		process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON = previousPolicy;
	}
}
