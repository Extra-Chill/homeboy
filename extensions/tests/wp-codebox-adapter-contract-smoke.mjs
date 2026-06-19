#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
	WP_CODEBOX_BACKEND,
	WP_CODEBOX_PROVIDER_ID,
	WP_CODEBOX_PROVIDER_LABEL,
	WP_CODEBOX_PROVIDER_RUNTIME_OPERATION_ALIASES,
	WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS,
	WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES,
	WP_CODEBOX_ROLE_ALIASES,
	WP_CODEBOX_TASK_REQUEST_SCHEMA,
	WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS,
	wpCodeboxProviderRuntimeInvocationContract,
	wpCodeboxProviderRuntimeOperationConfig,
	wpCodeboxProviderRuntimeOperationEntry,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'wp-codebox-adapter-contract.js'));

assert.equal(WP_CODEBOX_BACKEND, 'codebox');
assert.equal(WP_CODEBOX_PROVIDER_ID, 'wordpress.codebox-agent-task-executor');
assert.equal(WP_CODEBOX_PROVIDER_LABEL, 'WP Codebox agent task executor');
assert.equal(WP_CODEBOX_TASK_REQUEST_SCHEMA, 'wp-codebox/task-input/v1');

const invocationContract = wpCodeboxProviderRuntimeInvocationContract();
assert.equal(invocationContract.schema, 'wp-codebox/provider-runtime-invocation-contract/v1');
assert.deepEqual(invocationContract.tasks, WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES);
assert.deepEqual(invocationContract.result_schemas, WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS);

assert.equal(WP_CODEBOX_PROVIDER_RUNTIME_OPERATION_ALIASES.workspaceCapture, 'workspaceCapture');
assert.equal(WP_CODEBOX_PROVIDER_RUNTIME_OPERATION_ALIASES['wp-codebox/runner-workspace-command'], 'workspaceCommand');
assert.deepEqual(wpCodeboxProviderRuntimeOperationEntry('workspace_publication'), [
	'workspacePublish',
	{
		task: 'wp-codebox.runner-workspace.publish',
		ability: 'wp-codebox/runner-workspace-publish',
		result_schema: 'wp-codebox/runner-workspace-publication-result/v1',
	},
]);
assert.deepEqual(wpCodeboxProviderRuntimeOperationEntry({ ability: 'wp-codebox/handoff-artifacts', config: { required: true } }), [
	'artifactHandoff',
	{
		task: 'wp-codebox.artifact-handoff',
		ability: 'wp-codebox/handoff-artifacts',
		result_schema: 'wp-codebox/evidence-artifact-envelope/v1',
		config: { required: true },
	},
]);
assert.equal(wpCodeboxProviderRuntimeOperationEntry('unknown-operation'), null);
assert.deepEqual(wpCodeboxProviderRuntimeOperationConfig('toolCallTranscriptRecord'), {
	task: 'wp-codebox.tool-call-transcript.record',
	ability: 'wp-codebox/record-tool-call-transcript',
	result_schema: 'wp-codebox/tool-call-transcript/v1',
});

assert.deepEqual(WP_CODEBOX_ROLE_ALIASES.artifact_roles.patch, ['codebox-patch']);
assert.deepEqual(WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS.map((requirement) => requirement.id), [
	'runtime-profile',
	'parent-tool-bridge',
	'provider-runtime-invocation',
	'artifact-result-envelope',
]);

console.log('wp-codebox adapter contract smoke passed');
