#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(rootDir, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const { codeboxTaskRequestFromAgentTaskRequest } = require(
	path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'codebox-agent-task-executor.js')
);
const { codeboxRuntimeComponentContracts, codeboxRuntimeExtraPlugins, codeboxRuntimeProfilePayload } = require(
	path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'codebox-runtime-profile.js')
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

	const mergedProfile = codeboxRuntimeProfilePayload({
		profile: {
			id: 'shared-profile-helper',
			runtime_overlays: [{ kind: 'library', library: 'php-ai-client', source: '/runtime/php-ai-client' }],
			env: { PROFILE_ENV: '1', SHARED_ENV: 'profile' },
			provider_plugins: ['/runtime/provider-from-profile'],
		},
		runtimeRequirements: {
			runtime_overlays: [{ kind: 'plugin', slug: 'agents-api', source: '/runtime/agents-api' }],
			env: { REQUIREMENT_ENV: '1', SHARED_ENV: 'requirement' },
			provider_plugins: [{ path: '/runtime/provider-from-requirement' }],
		},
		runtimeOverlays: [{ kind: 'mu-plugin', slug: 'runtime-tools', source: '/runtime/runtime-tools' }],
		runtimeEnv: { REQUEST_ENV: '1', SHARED_ENV: 'request' },
		providerPluginPaths: ['/runtime/provider-from-request'],
	});
	assert.deepEqual(mergedProfile.runtime_overlays.map((overlay) => overlay.source), [
		'/runtime/php-ai-client',
		'/runtime/agents-api',
		'/runtime/runtime-tools',
	]);
	assert.deepEqual(mergedProfile.env, {
		PROFILE_ENV: '1',
		SHARED_ENV: 'request',
		REQUIREMENT_ENV: '1',
		REQUEST_ENV: '1',
	});
	assert.deepEqual(mergedProfile.provider_plugins, [
		{ path: '/runtime/provider-from-profile' },
		{ path: '/runtime/provider-from-requirement' },
		{ path: '/runtime/provider-from-request' },
	]);

	assert.deepEqual(codeboxRuntimeComponentContracts({
		profile: {
			components: [{ slug: 'runtime-tools', source: '/runtime/tools' }],
			plugins: [{ slug: 'runtime-provider', source: '/runtime/provider', activate: true }],
		},
		runtimeRequirements: {
			component_contracts: [{ slug: 'existing-contract', path: '/runtime/existing' }],
		},
		componentContracts: [{ slug: 'request-contract', path: '/runtime/request' }],
	}).map((contract) => ({ slug: contract.slug, path: contract.path, loadAs: contract.loadAs, activate: contract.activate })), [
		{ slug: 'existing-contract', path: '/runtime/existing', loadAs: undefined, activate: undefined },
		{ slug: 'runtime-tools', path: '/runtime/tools', loadAs: 'mu-plugin', activate: false },
		{ slug: 'runtime-provider', path: '/runtime/provider', loadAs: 'plugin', activate: true },
		{ slug: 'request-contract', path: '/runtime/request', loadAs: undefined, activate: undefined },
	]);
	assert.deepEqual(codeboxRuntimeExtraPlugins({
		profile: { extra_plugins: [{ slug: 'profile-plugin', source: '/runtime/profile-plugin' }] },
		componentContracts: [{ slug: 'request-contract', path: '/runtime/request' }],
	}).map((plugin) => plugin.slug), ['profile-plugin', 'request-contract']);

	console.log('wp-codebox runtime profile defaults smoke passed');
} finally {
	if (previousPolicy === undefined) {
		delete process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON;
	} else {
		process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON = previousPolicy;
	}
}
