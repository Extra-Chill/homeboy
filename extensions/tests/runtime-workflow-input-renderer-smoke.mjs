#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const packageRequire = createRequire(path.join(rootDir, 'runtime-agent-ci', 'package.json'));
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(rootDir, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const {
	renderRuntimeWorkflowInputs,
} = packageRequire('homeboy-runtime-agent-ci/runtime-workflow-inputs');

const rendered = renderRuntimeWorkflowInputs({
	runtime: 'wp-codebox',
	runtime_profile: 'example-runtime',
	runtime_profiles: {
		'example-runtime': {
			schema: 'homeboy/runtime-profile/v1',
			id: 'example-runtime',
			plugins: [
				{ slug: 'example-provider', source: '.ci/example-provider', activate: true },
			],
		},
	},
	tool_profile: {
		schema: 'homeboy/runtime-tool-profile/v1',
		tools: { workspace_read: true },
	},
});

assert.equal(rendered.schema, 'homeboy/runtime-workflow-inputs/v1');
assert.equal(rendered.runtime_id, 'wp-codebox');
assert.equal(rendered.runtime_profile, 'example-runtime');
assert.equal(rendered.runtime_requirements.schema, 'wp-codebox/runtime-profile/v1');
assert.equal(rendered.runtime_requirements.id, 'example-runtime');
assert.equal(rendered.runtime_requirements.homeboy_profile_schema, 'homeboy/runtime-profile/v1');
assert.equal(rendered.workflow_inputs.runtime, 'wp-codebox');
assert.equal(rendered.workflow_inputs.profile, 'example-runtime');
assert.deepEqual(rendered.workflow_inputs.sandbox_tool_policy, {
	schema: 'homeboy/runtime-tool-profile/v1',
	tools: { workspace_read: true },
});
assert.equal(rendered.workflow_inputs.runtime_profiles['example-runtime'].schema, 'wp-codebox/runtime-profile/v1');

const defaultRendered = renderRuntimeWorkflowInputs({
	runtime: 'example-runtime',
	runtime_profile: { id: 'example-profile', custom: true },
	runtimeProviderConfig: {
		id: 'example-runtime',
		executor: { backend: 'example-runtime' },
	},
});
assert.deepEqual(defaultRendered.workflow_inputs, {
	runtime: 'example-runtime',
	profile: 'example-profile',
	runtime_profiles: {
		'example-profile': { id: 'example-profile', custom: true },
	},
});
assert.deepEqual(defaultRendered.runtime_requirements, { id: 'example-profile', custom: true });

const runtimeMounts = [{ source: '/host/workload', target: '/runtime/workload', mode: 'readonly' }];
const objectProfileRendered = renderRuntimeWorkflowInputs({
	runtime: 'wp-codebox',
	runtime_profile: {
		id: 'object-profile',
		plugins: [{ slug: 'object-provider', source: '.ci/object-provider', activate: true }],
	},
	runtime_mounts: runtimeMounts,
});
assert.equal(objectProfileRendered.runtime_profile, 'object-profile');
assert.deepEqual(objectProfileRendered.runtime_requirements.runtime_mounts, runtimeMounts);
assert.deepEqual(objectProfileRendered.workflow_inputs.runtime_profiles['object-profile'].runtime_mounts, runtimeMounts);

const namedToolProfileRendered = renderRuntimeWorkflowInputs({
	runtime: 'wp-codebox',
	runtime_profile: 'example-runtime',
	runtime_profiles: {
		'example-runtime': { id: 'example-runtime' },
	},
	workload_profile: 'workspace_publication',
});
assert.equal(namedToolProfileRendered.workload_profile, 'workspace_publication');
assert.deepEqual(namedToolProfileRendered.tool_profile.publication_tools, [
	'publication_prepare',
	'publication_publish',
	'publication_status',
]);
assert.equal(namedToolProfileRendered.tool_profile.provider_runtime_invocation.operations.workspacePublish, true);
assert.deepEqual(namedToolProfileRendered.workflow_inputs.sandbox_tool_policy.publication_tools, namedToolProfileRendered.tool_profile.publication_tools);

const cliResult = spawnSync(process.execPath, [
	path.join(rootDir, 'runtime-agent-ci', 'scripts', 'render-runtime-workflow-inputs.cjs'),
	'--runtime', 'wp-codebox',
	'--runtime-profile', JSON.stringify({ id: 'cli-profile', custom: true }),
	'--runtime-mounts', JSON.stringify(runtimeMounts),
], { encoding: 'utf8' });
assert.equal(cliResult.status, 0, cliResult.stderr);
const cliRendered = JSON.parse(cliResult.stdout);
assert.equal(cliRendered.runtime_id, 'wp-codebox');
assert.equal(cliRendered.runtime_profile, 'cli-profile');
assert.deepEqual(cliRendered.runtime_requirements.runtime_mounts, runtimeMounts);

const actionScriptResult = spawnSync(process.execPath, [
	path.join(rootDir, '.github', 'scripts', 'runtime-agent-full-run', 'render-runtime-workflow-inputs.cjs'),
], {
	encoding: 'utf8',
		env: {
		...process.env,
		RUNTIME: 'wp-codebox',
		PROFILE: 'action-profile',
		RUNTIME_MOUNTS: JSON.stringify(runtimeMounts),
	},
});
assert.equal(actionScriptResult.status, 0, actionScriptResult.stderr);
const actionScriptRendered = JSON.parse(actionScriptResult.stdout);
assert.equal(actionScriptRendered.runtime_profile, 'action-profile');
assert.deepEqual(actionScriptRendered.runtime_requirements.runtime_mounts, runtimeMounts);

console.log('runtime workflow input renderer smoke passed');
