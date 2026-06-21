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
	WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA,
	WP_CODEBOX_PROVIDER_RUNTIME_OPERATION_ALIASES,
	WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS,
	WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES,
	WP_CODEBOX_RECIPE_RUN_CLI_COMMAND,
	WP_CODEBOX_ROLE_ALIASES,
	WP_CODEBOX_TASK_REQUEST_SCHEMA,
	WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS,
	WP_CODEBOX_WORKSPACE_MOUNT_KIND,
	WP_CODEBOX_LEGACY_WORKSPACE_MOUNT_KIND,
	wpCodeboxLegacyRuntimeComponentAliasDiagnostics,
	wpCodeboxLegacyRuntimeComponentAliasFields,
	wpCodeboxLegacyRuntimeComponentAliasValues,
	wpCodeboxProviderRuntimeInvocationContract,
	wpCodeboxProviderRuntimeOperationConfig,
	wpCodeboxProviderRuntimeOperationEntry,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'wp-codebox-adapter-contract.js'));
const {
	wpCodeboxBin,
	wpCodeboxCliDescriptor,
	wpCodeboxCommand,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'wp-codebox-adapter-descriptor.js'));

assert.equal(WP_CODEBOX_BACKEND, 'codebox');
assert.equal(WP_CODEBOX_PROVIDER_ID, 'wordpress.codebox-agent-task-executor');
assert.equal(WP_CODEBOX_PROVIDER_LABEL, 'WP Codebox agent task executor');
assert.equal(WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA, 'wp-codebox/provider-credential-boundary/v1');
assert.equal(WP_CODEBOX_TASK_REQUEST_SCHEMA, 'wp-codebox/task-input/v1');
assert.equal(WP_CODEBOX_RECIPE_RUN_CLI_COMMAND, 'recipe-run');
assert.equal(WP_CODEBOX_WORKSPACE_MOUNT_KIND, 'homeboy-runtime-workspace');
assert.equal(WP_CODEBOX_LEGACY_WORKSPACE_MOUNT_KIND, ['homeboy', 'dmc', 'workspace'].join('-'));

const cliDescriptor = wpCodeboxCliDescriptor();
assert.equal(cliDescriptor.schema, 'wp-codebox/cli-descriptor/v1');
assert.deepEqual(cliDescriptor.commands, {
	run_agent_task: 'run-agent-task',
	legacy_agent_task_run: 'agent-task-run',
	recipe_run: 'recipe-run',
});
assert.equal(wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_BIN: '/usr/local/bin/wp-codebox' } }), '/usr/local/bin/wp-codebox');
assert.equal(wpCodeboxBin({ settings: { wp_codebox_bin: '/settings/wp-codebox' } }), '/settings/wp-codebox');
assert.equal(wpCodeboxBin({ executable: '' }), undefined);
assert.deepEqual(wpCodeboxCommand('/tmp/wp-codebox.mjs'), { command: process.execPath, args: ['/tmp/wp-codebox.mjs'] });

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
	'run-agent-task',
	'provider-credential-boundary',
	'runtime-profile',
	'parent-tool-bridge',
	'provider-runtime-invocation',
	'artifact-result-envelope',
	'artifact-apply-execution',
]);
assert.equal(
	WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS.find((requirement) => requirement.id === 'artifact-result-envelope').adapter_behavior,
	'consume_canonical_envelope_with_legacy_package_fallback'
);
assert.doesNotMatch(JSON.stringify(cliDescriptor), /datamachine|data machine|wp-site-generator|wpsg|site generator/i);

const legacyRuntimeAliasPrefix = ['data', 'machine'].join('_');
assert.deepEqual(wpCodeboxLegacyRuntimeComponentAliasFields(), {
	agent_runtime: [legacyRuntimeAliasPrefix, `${legacyRuntimeAliasPrefix}_path`],
	agent_runtime_tools: [`${legacyRuntimeAliasPrefix}_code`, `${legacyRuntimeAliasPrefix}_code_path`],
});
assert.deepEqual(wpCodeboxLegacyRuntimeComponentAliasValues({
	[legacyRuntimeAliasPrefix]: '/runtime',
	[`${legacyRuntimeAliasPrefix}_code_path`]: '/runtime-tools',
}), {
	agent_runtime: '/runtime',
	agent_runtime_tools: '/runtime-tools',
});
assert.deepEqual(
	wpCodeboxLegacyRuntimeComponentAliasDiagnostics({ [`${legacyRuntimeAliasPrefix}_path`]: '/runtime' }).map((diagnostic) => diagnostic.data.replacement),
	['agent_runtime']
);

console.log('wp-codebox adapter contract smoke passed');
