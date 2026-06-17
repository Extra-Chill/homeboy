'use strict';

const assert = require('node:assert/strict');
const {
	WORKLOAD_PROFILE_SCHEMA,
	normalizeWordPressWorkloadProfile,
	workflowInputsFromWordPressWorkloadProfile,
} = require('../lib/wordpress-workload-profile');

const profile = {
	schema: WORKLOAD_PROFILE_SCHEMA,
	id: 'static-import-visual-check',
	label: 'Static import visual check',
	dependencies: [
		'example/static-importer@main',
		{ repo: 'example/block-transformer', ref: 'trunk' },
	],
	wp_config_defines: { WP_DEBUG: true },
	mounts: [
		'/tmp/source:/wordpress/wp-content/uploads/source:readonly',
		{ source: '/tmp/drop-in.php', target: '/wordpress/wp-content/db.php', mode: 'readonly' },
	],
	run_before: [
		{ type: 'wp-cli', command: 'plugin install safe-svg --activate' },
	],
	workloads: [
		{
			id: 'import-and-snapshot',
			label: 'Import and snapshot',
			run: [
				{ type: 'ability', ability: 'example/import-static-site', input: { source: '/wordpress/wp-content/uploads/source' } },
			],
			artifacts: {
				import_report: { path: 'wp-content/uploads/import-report.json', kind: 'json' },
			},
		},
	],
	run_after: [
		{ type: 'wp-cli', command: 'option get home' },
	],
	visual_comparisons: [
		{
			id: 'home-page',
			source_url: 'https://source.example/',
			candidate_url: 'https://candidate.example/',
			threshold: 0.01,
		},
	],
	metadata: { fixture: 'portable' },
};

const normalized = normalizeWordPressWorkloadProfile(profile);
assert.equal(normalized.schema, WORKLOAD_PROFILE_SCHEMA);
assert.equal(normalized.dependencies[1].entry, 'example/block-transformer@trunk');
assert.equal(normalized.mounts[0].mode, 'readonly');
assert.equal(normalized.visual_comparisons[0].artifacts_directory, 'artifacts/visual/home-page');

const inputs = workflowInputsFromWordPressWorkloadProfile(profile);
assert.equal(inputs.validation_dependencies, 'example/static-importer@main,example/block-transformer@trunk');
assert.deepEqual(JSON.parse(inputs.extra_wp_config_defines), { WP_DEBUG: true });
assert.deepEqual(JSON.parse(inputs.runtime_mounts), normalized.mounts);
assert.deepEqual(JSON.parse(inputs.workload_run_before), normalized.run_before);
assert.deepEqual(JSON.parse(inputs.wp_codebox_workloads), normalized.workloads);
assert.deepEqual(JSON.parse(inputs.workload_run_after), [
	{ type: 'wp-cli', command: 'option get home' },
	{
		type: 'visual-compare',
		id: 'home-page',
		source_url: 'https://source.example/',
		candidate_url: 'https://candidate.example/',
		threshold: 0.01,
		artifacts_directory: 'artifacts/visual/home-page',
	},
]);
assert.deepEqual(inputs.metadata, {
	profile_id: 'static-import-visual-check',
	profile_label: 'Static import visual check',
	fixture: 'portable',
});

assert.throws(() => normalizeWordPressWorkloadProfile({ id: 'bad', workloads: [{ id: 'empty' }] }), /run must contain/);
assert.throws(() => normalizeWordPressWorkloadProfile({ schema: 'other/v1', id: 'bad' }), /Unsupported/);

console.log('wordpress workload profile smoke passed');
