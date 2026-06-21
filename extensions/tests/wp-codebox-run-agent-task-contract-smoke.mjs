#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
	WP_CODEBOX_LEGACY_AGENT_TASK_RUN_CLI_COMMAND,
	WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA,
	WP_CODEBOX_AGENT_TASK_RUN_RESPONSE_SCHEMA,
	WP_CODEBOX_RUN_AGENT_TASK_CLI_COMMAND,
	WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA,
	WP_CODEBOX_RUN_AGENT_TASK_RESULT_SCHEMA,
	codeboxRunAgentTaskInvocation,
	codeboxRunAgentTaskRequestFromTaskInput,
	isCodeboxLegacyAgentTaskRunResult,
	legacyAgentTaskRunEvidenceRefs,
	legacyAgentTaskRunSessionArtifacts,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox'));

const taskInput = {
	schema: 'wp-codebox/task-input/v1',
	sandbox_session_id: 'task-123',
	artifacts_path: '/tmp/artifacts',
	provider: 'codex',
};

const request = codeboxRunAgentTaskRequestFromTaskInput(taskInput);
assert.equal(request.schema, WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA);
assert.equal(request.version, 1);
assert.equal(request.task_id, 'task-123');
assert.equal(request.task_input, taskInput);
assert.deepEqual(request.compatibility, {
	legacy_input_schema: 'wp-codebox/task-input/v1',
	legacy_cli_command: WP_CODEBOX_LEGACY_AGENT_TASK_RUN_CLI_COMMAND,
});

const legacyInvocation = codeboxRunAgentTaskInvocation({
	taskInput,
	previewHold: '30',
	previewPublicUrl: 'https://preview.example.test',
});
assert.equal(legacyInvocation.contract, WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA);
assert.equal(legacyInvocation.implementation, 'legacy-agent-task-run-compat');
assert.equal(legacyInvocation.input, taskInput);
assert.deepEqual(legacyInvocation.args, [
	WP_CODEBOX_LEGACY_AGENT_TASK_RUN_CLI_COMMAND,
	'--input-file={{input_file}}',
	'--json',
	'--preview-hold-seconds=30',
	'--preview-public-url=https://preview.example.test',
]);

const stableInvocation = codeboxRunAgentTaskInvocation({ taskInput, useStableRunAgentTask: true });
assert.equal(stableInvocation.implementation, 'stable-run-agent-task');
assert.equal(stableInvocation.input.schema, WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA);
assert.equal(stableInvocation.args[0], WP_CODEBOX_RUN_AGENT_TASK_CLI_COMMAND);
assert.equal(stableInvocation.result_schema, WP_CODEBOX_RUN_AGENT_TASK_RESULT_SCHEMA);
assert.equal(stableInvocation.result_schema, WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA);
assert.equal(stableInvocation.result_key, 'agent_task_run_result');

assert.throws(
	() => codeboxRunAgentTaskRequestFromTaskInput({ schema: 'wp-codebox/not-task-input/v1' }),
	/wp-codebox\/task-input\/v1/
);

const legacyResult = {
	schema: WP_CODEBOX_AGENT_TASK_RUN_RESPONSE_SCHEMA,
	artifacts: '/tmp/artifacts',
	session: {
		id: 'session-1',
		artifacts: {
			bundle_id: 'bundle-1',
			preview_url: 'https://preview.example.test',
		},
	},
};
assert.equal(isCodeboxLegacyAgentTaskRunResult(legacyResult), true);
assert.equal(isCodeboxLegacyAgentTaskRunResult({ schema: WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA }), false);
assert.deepEqual(legacyAgentTaskRunSessionArtifacts(legacyResult), [
	{
		id: 'bundle-1',
		kind: 'codebox-artifact-directory',
		path: '/tmp/artifacts',
		metadata: {
			session_id: 'session-1',
			preview_url: 'https://preview.example.test',
		},
	},
	{
		id: 'bundle-1',
		kind: 'codebox-session-artifacts',
		url: 'https://preview.example.test',
		metadata: legacyResult.session.artifacts,
	},
]);
assert.deepEqual(legacyAgentTaskRunEvidenceRefs(legacyResult), [
	{
		kind: 'codebox-preview',
		uri: 'https://preview.example.test',
		label: 'WP Codebox preview',
	},
	{
		kind: 'codebox-artifact-directory',
		uri: '/tmp/artifacts',
		label: 'WP Codebox artifacts',
	},
]);

console.log('wp-codebox run-agent-task contract smoke passed');
