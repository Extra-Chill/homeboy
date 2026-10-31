'use strict';

const assert = require('node:assert/strict');
const {
	WORKLOAD_PROFILE_SCHEMA,
	WORDPRESS_WORKLOAD_ARTIFACT_DECLARATION_SCHEMA,
	normalizeArtifactDeclarations,
	normalizeWordPressWorkloadProfile,
	workflowInputsFromWordPressWorkloadProfile,
} = require('../lib/wordpress-workload-profile');

const profile = {
	schema: WORKLOAD_PROFILE_SCHEMA,
	id: 'content-import-visual-check',
	label: 'Content import visual check',
	dependencies: [
		'example/content-importer@main',
		{ repo: 'example/block-transformer', ref: 'trunk' },
	],
	wp_config_defines: { WP_DEBUG: true },
	mounts: [
		'/tmp/source:/wordpress/wp-content/uploads/source:readonly',
		{ source: '/tmp/drop-in.php', target: '/wordpress/wp-content/db.php', mode: 'readonly' },
	],
	fixture_plugins: [
		{ path: '/tmp/fixture-plugin', plugin: 'fixture-plugin/fixture-plugin.php' },
	],
	fixture_site_seeds: {
		name: 'minimal-content',
		posts: { postTypes: ['page'], maxRecords: 1 },
		options: { names: ['blogname'] },
	},
	fixtures: [
		{ id: 'seed-page', path: 'fixtures/seed-page.php' },
	],
	run_before: [
		{ type: 'wp-cli', command: 'plugin install safe-svg --activate' },
	],
	workloads: [
		{
			id: 'import-and-snapshot',
			label: 'Import and snapshot',
			run: [
				{ type: 'ability', ability: 'example/import-content', input: { source: '/wordpress/wp-content/uploads/source' } },
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
	artifact_declarations: {
		import_report: { path: 'wp-content/uploads/import-report.json', kind: 'json', role: 'report' },
	},
	metadata: { fixture: 'portable' },
};

const normalized = normalizeWordPressWorkloadProfile(profile);
assert.equal(normalized.schema, WORKLOAD_PROFILE_SCHEMA);
assert.equal(normalized.dependencies[1].entry, 'example/block-transformer@trunk');
assert.equal(normalized.mounts[0].mode, 'readonly');
assert.equal(normalized.fixture_plugins[0].schema, 'homeboy/wordpress-fixture-plugin/v1');
assert.equal(normalized.fixture_site_seeds[0].schema, 'homeboy/wordpress-fixture-site-seed/v1');
assert.equal(normalized.fixtures[0].schema, 'homeboy/wordpress-fixture-step/v1');
assert.equal(normalized.visual_comparisons[0].artifacts_directory, 'artifacts/visual/home-page');
assert.equal(normalized.artifact_declarations.import_report.schema, WORDPRESS_WORKLOAD_ARTIFACT_DECLARATION_SCHEMA);
assert.equal(normalized.artifact_declarations.import_report.required, true);

const inputs = workflowInputsFromWordPressWorkloadProfile(profile);
assert.equal(inputs.validation_dependencies, 'example/content-importer@main,example/block-transformer@trunk');
assert.deepEqual(JSON.parse(inputs.extra_wp_config_defines), { WP_DEBUG: true });
assert.deepEqual(JSON.parse(inputs.runtime_mounts), normalized.mounts);
assert.deepEqual(JSON.parse(inputs.wordpress_fixture_plugins), normalized.fixture_plugins);
assert.deepEqual(JSON.parse(inputs.wordpress_fixture_site_seeds), normalized.fixture_site_seeds);
assert.deepEqual(JSON.parse(inputs.wordpress_fixture_steps), normalized.fixtures);
assert.deepEqual(JSON.parse(inputs.workload_run_before), normalized.run_before);
assert.deepEqual(JSON.parse(inputs.wordpress_runtime_workloads), normalized.workloads);
assert.equal(Object.hasOwn(inputs, 'wp_codebox_workloads'), false);
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
assert.deepEqual(JSON.parse(inputs.artifact_declarations), normalized.artifact_declarations);
assert.deepEqual(inputs.metadata, {
	profile_id: 'content-import-visual-check',
	profile_label: 'Content import visual check',
	fixture: 'portable',
});

assert.deepEqual(normalizeArtifactDeclarations({ summary: { path: 'artifacts/summary.json' } }).summary, {
	schema: WORDPRESS_WORKLOAD_ARTIFACT_DECLARATION_SCHEMA,
	id: 'summary',
	path: 'artifacts/summary.json',
	kind: 'file',
	required: true,
	role: 'summary',
	metadata: {},
});

assert.throws(() => normalizeWordPressWorkloadProfile({ id: 'bad', workloads: [{ id: 'empty' }] }), /run must contain/);
assert.throws(() => normalizeWordPressWorkloadProfile({ schema: 'other/v1', id: 'bad' }), /Unsupported/);
assert.throws(() => normalizeArtifactDeclarations({ bad: { kind: 'json' } }), /path/);

console.log('wordpress workload profile smoke passed');
