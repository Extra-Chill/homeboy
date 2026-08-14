#!/usr/bin/env node
import assert from 'node:assert/strict';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const wordpressManifest = JSON.parse(
	readFileSync(path.join(rootDir, 'wordpress', 'homeboy.json'), 'utf8')
);
const runtimeManifest = JSON.parse(
	readFileSync(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'wp-codebox.json'), 'utf8')
);
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(rootDir, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
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
	wpCodeboxProviderRuntimeInvocationContract,
	wpCodeboxProviderRuntimeOperationConfig,
	wpCodeboxProviderRuntimeOperationEntry,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'wp-codebox-adapter-contract.js'));
const {
	WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA,
	codeboxRunAgentTaskInvocation,
	codeboxRunAgentTaskRequestFromTaskInput,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'codebox-run-agent-task-contract.js'));
const {
	wpCodeboxBin,
	wpCodeboxCliDescriptor,
	wpCodeboxCommand,
	wpCodeboxRuntimePackageSourceDescriptor,
} = require(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'wp-codebox-adapter-descriptor.js'));

assert.equal(WP_CODEBOX_BACKEND, 'wp-codebox');
assert.equal(WP_CODEBOX_PROVIDER_ID, 'wordpress.codebox-agent-task-executor');
assert.equal(WP_CODEBOX_PROVIDER_LABEL, 'WP Codebox agent task executor');
assert.equal(WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA, 'wp-codebox/provider-credential-boundary/v1');
assert.equal(WP_CODEBOX_TASK_REQUEST_SCHEMA, 'wp-codebox/task-input/v1');
assert.equal(WP_CODEBOX_RECIPE_RUN_CLI_COMMAND, 'recipe-run');
assert.equal(WP_CODEBOX_WORKSPACE_MOUNT_KIND, 'homeboy-runtime-workspace');

const cliDescriptor = wpCodeboxCliDescriptor();
assert.equal(cliDescriptor.schema, 'wp-codebox/cli-descriptor/v1');
const recipeRunProviders = wordpressManifest.recipe_run_providers.filter(
	(provider) => provider.id === 'wordpress.wp-codebox.recipe-run'
);
assert.equal(recipeRunProviders.length, 1);
const [recipeRunProvider] = recipeRunProviders;
assert.deepEqual(recipeRunProvider, {
	id: 'wordpress.wp-codebox.recipe-run',
	version: '1.5.3',
	executable: cliDescriptor.executable,
	command: [
		cliDescriptor.executable,
		cliDescriptor.commands.recipe_run,
		'--recipe',
		'{recipe}',
		'--artifacts',
		'{artifacts}',
		'--json',
	],
});
assert.equal(runtimeManifest.recipe_run_providers, undefined);
assert.deepEqual(cliDescriptor.commands, {
	run_agent_task: 'run-agent-task',
	recipe_run: 'recipe-run',
});
const emptyManagedInstallDir = path.join(tmpdir(), 'homeboy-wp-codebox-empty-managed');
assert.equal(wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_INSTALL_DIR: emptyManagedInstallDir, HOMEBOY_WP_CODEBOX_BIN: '/usr/local/bin/wp-codebox' } }), '/usr/local/bin/wp-codebox');
assert.equal(wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_INSTALL_DIR: emptyManagedInstallDir }, settings: { wp_codebox_bin: '/settings/wp-codebox' } }), '/settings/wp-codebox');
assert.deepEqual(wpCodeboxCommand('/tmp/wp-codebox.mjs'), { command: process.execPath, args: ['/tmp/wp-codebox.mjs'] });
assert.deepEqual(wpCodeboxRuntimePackageSourceDescriptor('@extra-chill/release-runtime').descriptor, { slug: 'release-runtime', source: '@extra-chill/release-runtime' });
assert.deepEqual(wpCodeboxRuntimePackageSourceDescriptor({ slug: 'local-runtime', source: '/local/wp-codebox/runtime' }).descriptor, { slug: 'local-runtime', source: '/local/wp-codebox/runtime' });
assert.deepEqual(wpCodeboxRuntimePackageSourceDescriptor({ slug: 'cached-runtime', path: '/cache/homeboy/wp-codebox/source' }).descriptor, { slug: 'cached-runtime', path: '/cache/homeboy/wp-codebox/source' });

const fixtureRoot = mkdtempSync(path.join(tmpdir(), 'homeboy-wp-codebox-bin-'));
try {
	assert.equal(wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_INSTALL_DIR: path.join(fixtureRoot, 'empty-managed') }, executable: '' }), undefined);

	const runtimeComponent = path.join(fixtureRoot, 'packages', 'wordpress-plugin');
	const runtimeCli = path.join(fixtureRoot, 'packages', 'cli', 'dist', 'index.js');
	mkdirSync(runtimeComponent, { recursive: true });
	mkdirSync(path.dirname(runtimeCli), { recursive: true });
	writeFileSync(runtimeCli, '#!/usr/bin/env node\n');
	chmodSync(runtimeCli, 0o755);
	assert.equal(
		wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT: runtimeComponent }, executable: '' }),
		runtimeCli
	);
	assert.equal(
		wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT: runtimeComponent }, wp_codebox_bin: '/path/default/wp-codebox', executable: '', preferPackagedRuntime: true }),
		runtimeCli
	);
	assert.equal(
		wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT: runtimeComponent }, wpCodeboxBin: '/path/request/wp-codebox', executable: '', preferPackagedRuntime: true }),
		runtimeCli
	);
	assert.equal(
		wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT: runtimeComponent }, runtime_bin: '/path/runtime/wp-codebox', executable: '', preferPackagedRuntime: true }),
		runtimeCli
	);
	assert.equal(
		wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT: runtimeComponent }, wp_codebox_bin: '/path/explicit/wp-codebox', executable: '' }),
		'/path/explicit/wp-codebox'
	);

	const managedCli = path.join(fixtureRoot, 'managed', 'source', 'packages', 'cli', 'dist', 'index.js');
	mkdirSync(path.dirname(managedCli), { recursive: true });
	writeFileSync(managedCli, '#!/usr/bin/env node\n');
	chmodSync(managedCli, 0o755);
	assert.equal(
		wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_INSTALL_DIR: path.join(fixtureRoot, 'managed') }, executable: '' }),
		managedCli
	);
	assert.equal(
		wpCodeboxBin({
			env: {
				HOMEBOY_WP_CODEBOX_INSTALL_DIR: path.join(fixtureRoot, 'managed'),
				HOMEBOY_WP_CODEBOX_BIN: '/stale/env/wp-codebox',
			},
			settings: { wp_codebox_bin: '/stale/settings/wp-codebox' },
			executable: '',
		}),
		managedCli
	);
	assert.equal(
		wpCodeboxBin({ env: { HOMEBOY_WP_CODEBOX_INSTALL_DIR: path.join(fixtureRoot, 'managed') }, runtime_bin: '/fresh/runtime/wp-codebox', executable: '' }),
		'/fresh/runtime/wp-codebox'
	);
} finally {
	rmSync(fixtureRoot, { recursive: true, force: true });
}

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
const upstreamPrimitiveIds = WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS.map((requirement) => requirement.id);
for (const requiredPrimitiveId of [
	'run-agent-task',
	'provider-credential-boundary',
	'runtime-profile',
	'parent-tool-bridge',
	'provider-runtime-invocation',
	'artifact-result-envelope',
	'artifact-apply-execution',
	'preview-materialization',
]) {
	assert.ok(upstreamPrimitiveIds.includes(requiredPrimitiveId), `${requiredPrimitiveId} remains declared as an upstream primitive`);
}
assert.equal(
	WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS.find((requirement) => requirement.id === 'artifact-result-envelope').adapter_behavior,
	'consume_canonical_public_envelope_only'
);
assert.equal(
	WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS.find((requirement) => requirement.id === 'preview-materialization').adapter_behavior,
	'delegate_contained_site_open_without_constructing_playground_urls'
);
assert.doesNotMatch(JSON.stringify(cliDescriptor), /datamachine|data machine|wp-site-generator|wpsg|site generator/i);

const taskInput = {
	schema: WP_CODEBOX_TASK_REQUEST_SCHEMA,
	sandbox_session_id: 'task-123',
	artifacts_path: '/tmp/artifacts',
	provider: 'codex',
};
const request = codeboxRunAgentTaskRequestFromTaskInput(taskInput);
assert.equal(request.schema, WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA);
assert.equal(request.task_id, 'task-123');
assert.equal(request.task_input, taskInput);

const invocation = codeboxRunAgentTaskInvocation({ taskInput });
assert.equal(invocation.implementation, 'stable-run-agent-task');
assert.deepEqual(invocation.args, ['run-agent-task', '--input-file={{input_file}}', '--json']);

console.log('wp-codebox adapter contract smoke passed');
