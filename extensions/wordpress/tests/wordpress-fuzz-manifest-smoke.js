'use strict';

const assert = require('node:assert/strict');
const {
	WORDPRESS_FUZZ_MANIFEST_SCHEMA,
	normalizeWordPressFuzzManifest,
	workflowInputsFromWordPressFuzzManifest,
} = require('../lib/wordpress-fuzz-manifest');

const manifest = {
	schema: WORDPRESS_FUZZ_MANIFEST_SCHEMA,
	id: 'generic-rest-fuzz',
	label: 'Generic REST fuzz',
	dependencies: ['example/plugin-under-test@main'],
	mounts: ['/tmp/corpus:/wordpress/wp-content/uploads/corpus:readonly'],
	run_before: [{ type: 'wp-cli', command: 'rewrite flush' }],
	workloads: [
		{
			id: 'execute-fuzz-plan',
			run: [
				{ type: 'ability', ability: 'wordpress/fuzz-run', input: { plan_id: 'generic-rest-plan' } },
			],
		},
	],
	discovery: {
		id: 'generic-surfaces',
		surfaces: [
			{ type: 'rest-route', id: 'wp-v2-posts', method: 'GET', route: '/wp/v2/posts' },
		],
	},
	plan: {
		id: 'generic-rest-plan',
		discovery_id: 'generic-surfaces',
		targets: [
			{
				id: 'posts-list',
				surface_id: 'wp-v2-posts',
				cases: [{ id: 'per-page-boundary', query: { per_page: 100 } }],
			},
		],
	},
	artifacts: [{ path: 'artifacts/fuzz/result.json', kind: 'json' }],
	budget: { max_cases: 25 },
	metadata: { fixture: 'portable' },
};

const normalized = normalizeWordPressFuzzManifest(manifest);
assert.equal(normalized.schema, WORDPRESS_FUZZ_MANIFEST_SCHEMA);
assert.equal(normalized.workload_profile.id, 'generic-rest-fuzz');
assert.equal(normalized.workload_profile.dependencies[0].entry, 'example/plugin-under-test@main');
assert.equal(normalized.workload_profile.mounts[0].mode, 'readonly');
assert.equal(normalized.discovery.schema, 'wordpress-surface-discovery/v1');
assert.equal(normalized.plan.schema, 'wordpress-fuzz-plan/v1');
assert.equal(normalized.plan.targets[0].cases[0].id, 'per-page-boundary');
assert.deepEqual(normalized.budget, { max_cases: 25 });

const inputs = workflowInputsFromWordPressFuzzManifest(manifest);
assert.equal(inputs.validation_dependencies, 'example/plugin-under-test@main');
assert.deepEqual(JSON.parse(inputs.wordpress_runtime_workloads), normalized.workload_profile.workloads);
assert.deepEqual(JSON.parse(inputs.wordpress_fuzz_discovery), normalized.discovery);
assert.deepEqual(JSON.parse(inputs.wordpress_fuzz_plan), normalized.plan);
assert.deepEqual(JSON.parse(inputs.wordpress_fuzz_artifacts), normalized.artifacts);
assert.equal(JSON.parse(inputs.wordpress_fuzz_manifest).id, 'generic-rest-fuzz');
assert.deepEqual(inputs.metadata, {
	profile_id: 'generic-rest-fuzz',
	profile_label: 'Generic REST fuzz',
	fixture: 'portable',
	manifest_id: 'generic-rest-fuzz',
	manifest_label: 'Generic REST fuzz',
});

assert.throws(() => normalizeWordPressFuzzManifest({ schema: 'other/v1' }), /Unsupported/);
assert.throws(() => normalizeWordPressFuzzManifest({ id: 'empty', workloads: [{ id: 'missing-run' }] }), /run must contain/);

console.log('wordpress fuzz manifest smoke passed');
