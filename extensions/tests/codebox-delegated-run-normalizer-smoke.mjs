#!/usr/bin/env node
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
	DELEGATED_RUN_REQUEST_SCHEMA,
	DELEGATED_RUN_RESULT_SCHEMA,
	normalizeDelegatedRunRequest,
	normalizeDelegatedRunResult,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox'));

const commandRequest = normalizeDelegatedRunRequest({
	schema: 'homeboy/agent-task-request/v1',
	task_id: 'delegated-command-smoke',
	executor: {
		backend: 'codebox',
		config: {
			delegated_run: {
				type: 'command',
				argv: ['npm', 'test'],
				cwd: 'workspace',
				env: { NODE_ENV: 'test' },
				metadata: { visible: true, secret_token: 'must-not-leak' },
			},
		},
	},
	instructions: 'Run the requested command.',
	workspace: { root: '/workspace/project' },
});

assert.equal(commandRequest.schema, DELEGATED_RUN_REQUEST_SCHEMA);
assert.equal(commandRequest.id, 'delegated-command-smoke');
assert.deepEqual(commandRequest.execution, {
	type: 'command',
	argv: ['npm', 'test'],
	cwd: 'workspace',
	env_names: ['NODE_ENV'],
});
assert.equal(commandRequest.metadata.visible, true);
assert.equal(Object.hasOwn(commandRequest.metadata, 'secret_token'), false);

const agentRequest = normalizeDelegatedRunRequest({
	schema: 'homeboy/agent-task-request/v1',
	task_id: 'delegated-agent-smoke',
	executor: { backend: 'codebox' },
	instructions: 'Inspect the workspace and report findings.',
	tools: ['workspace_read'],
	inputs: {
		delegatedRun: {
			type: 'agent_run',
			agent: 'reviewer',
			input: { scope: 'changed-files' },
		},
	},
});

assert.equal(agentRequest.execution.type, 'agent_run');
assert.equal(agentRequest.execution.agent, 'reviewer');
assert.equal(agentRequest.execution.instructions, 'Inspect the workspace and report findings.');
assert.deepEqual(agentRequest.execution.tools, ['workspace_read']);
assert.deepEqual(agentRequest.input, { scope: 'changed-files' });

const result = normalizeDelegatedRunResult({
	task_id: 'delegated-command-smoke',
	status: 'completed',
	outputs: { changed: false },
	artifacts: [{ path: '/tmp/run.json', metadata: { token: 'must-not-leak', visible: true } }],
	diagnostics: ['finished'],
	metadata: { credential: 'must-not-leak', visible: true },
});

assert.equal(result.schema, DELEGATED_RUN_RESULT_SCHEMA);
assert.equal(result.status, 'succeeded');
assert.deepEqual(result.outputs, { changed: false });
assert.equal(result.artifacts[0].kind, 'delegated-run-artifact');
assert.equal(result.artifacts[0].metadata.visible, true);
assert.equal(Object.hasOwn(result.artifacts[0].metadata, 'token'), false);
assert.equal(result.diagnostics[0].class, 'delegated_run');
assert.equal(Object.hasOwn(result.metadata, 'credential'), false);

assert.equal(normalizeDelegatedRunRequest({
	schema: 'homeboy/agent-task-request/v1',
	task_id: 'no-delegated-run',
	executor: { backend: 'codebox' },
	instructions: 'No delegated run requested.',
}), null);

console.log('codebox delegated run normalizer smoke passed');
