#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
	renderRuntimeWorkflowInputs,
} = require(path.join(rootDir, 'runtime-agent-ci', 'lib', 'runtime-workflow-inputs.cjs'));

const rendered = renderRuntimeWorkflowInputs({
	runtime: 'codebox',
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
	runtimeProviderConfig: {
		id: 'wp-codebox',
		executor: { backend: 'codebox' },
	},
});

assert.equal(rendered.schema, 'homeboy/runtime-workflow-inputs/v1');
assert.equal(rendered.runtime_id, 'wp-codebox');
assert.equal(rendered.runtime_profile, 'example-runtime');
assert.equal(rendered.runtime_requirements.schema, 'wp-codebox/runtime-profile/v1');
assert.equal(rendered.runtime_requirements.id, 'example-runtime');
assert.equal(rendered.runtime_requirements.homeboy_profile_schema, 'homeboy/runtime-profile/v1');
assert.deepEqual(rendered.runtime_requirements.component_contracts.map((contract) => ({
	slug: contract.slug,
	path: contract.path,
	loadAs: contract.loadAs,
	activate: contract.activate,
})), [
	{ slug: 'example-provider', path: '.ci/example-provider', loadAs: 'plugin', activate: true },
]);
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

console.log('runtime workflow input renderer smoke passed');
