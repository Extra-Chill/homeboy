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
	console.log('wp-codebox runtime profile defaults smoke passed');
} finally {
	if (previousPolicy === undefined) {
		delete process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON;
	} else {
		process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON = previousPolicy;
	}
}
