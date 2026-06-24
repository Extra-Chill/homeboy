'use strict';

/* eslint-disable no-console */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	WEBSITE_ARTIFACT_SCHEMA,
	buildStaticSiteFixtureArtifact,
	buildStaticSiteFixtureMatrixRecipe,
	classifyStaticSiteFinding,
	collectStaticSiteFixtureMatrixRunResults,
	createStaticSiteFixtureMatrix,
	discoverStaticSiteFixtures,
	normalizeStaticSiteFixtureMatrixResult,
	writeStaticSiteFixtureMatrixResultArtifacts,
	writeStaticSiteFixtureMatrixArtifacts,
} = require('../lib/static-site-fixture-matrix');

function write(filePath, contents) {
	fs.mkdirSync(path.dirname(filePath), { recursive: true });
	fs.writeFileSync(filePath, contents);
}

function readJson(filePath) {
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

async function main() {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-static-site-fixture-matrix-'));
	try {
		const bonsai = path.join(root, 'saveweb2zip-com-liquidbonsai-com');
		const studio = path.join(root, '41-generative-art-studio');
		write(path.join(bonsai, 'index.html'), '<canvas id="canvas"></canvas><script src="js/script.js"></script>');
		write(path.join(bonsai, 'js', 'script.js'), "document.getElementById('canvas').getContext('2d');");
		write(path.join(bonsai, 'images', 'og-image.png'), 'png');
		write(path.join(studio, 'index.html'), '<button>Launch</button><img src="missing.svg">');
		write(path.join(studio, 'style.css'), 'button { color: white; }');

		const fixtures = discoverStaticSiteFixtures(root);
		assert.deepEqual(fixtures.map((fixture) => fixture.id), [
			'41-generative-art-studio',
			'saveweb2zip-com-liquidbonsai-com',
		]);

		const matrix = createStaticSiteFixtureMatrix({ fixture_root: root, id: 'fixture-run' });
		assert.equal(matrix.schema, 'homeboy/static-site-fixture-matrix/v1');
		assert.equal(matrix.count, 2);

		const artifact = buildStaticSiteFixtureArtifact(matrix.fixtures.find((fixture) => fixture.id.includes('liquidbonsai')));
		assert.equal(artifact.schema, WEBSITE_ARTIFACT_SCHEMA);
		assert.equal(artifact.entrypoint, 'website/index.html');
		assert.equal(artifact.summary.has_js, true);
		assert.equal(artifact.summary.has_images, true);
		assert.ok(artifact.files.some((file) => file.path === 'website/index.html'));
		assert.ok(artifact.files.some((file) => file.path === 'website/js/script.js'));

		const recipe = buildStaticSiteFixtureMatrixRecipe({ matrix, artifactsDirectory: '/artifacts/matrix' });
		assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
		assert.equal(Object.hasOwn(recipe, 'static_site_fixture_matrix'), false);
		assert.equal(recipe.workflow.steps.length, 2);
		assert.equal(recipe.workflow.steps[0].command, 'wordpress.wp-cli');
		assert.equal(Object.hasOwn(recipe.workflow.steps[0], 'metadata'), false);
		assert.match(recipe.workflow.steps[0].args[0], /command=static-site-importer validate-in-codebox/);
		assert.match(recipe.workflow.steps[0].args[0], /--artifact=\/artifacts\/matrix\/41-generative-art-studio\/artifact.json/);

		const staticSiteImporterRecipe = buildStaticSiteFixtureMatrixRecipe({
			matrix,
			artifactsDirectory: '/artifacts/matrix',
			staticSiteImporterPath: '/workspace/static-site-importer',
			staticSiteImporterPlugin: 'static-site-importer/static-site-importer.php',
			staticSiteImporterSlug: 'static-site-importer',
		});
		assert.deepEqual(Object.keys(staticSiteImporterRecipe).sort(), ['artifacts', 'inputs', 'runtime', 'schema', 'workflow']);
		assert.deepEqual(staticSiteImporterRecipe.inputs.extra_plugins[0], {
			source: '/workspace/static-site-importer',
			slug: 'static-site-importer',
			activate: true,
		});
		assert.equal(staticSiteImporterRecipe.workflow.steps[0].command, 'wordpress.wp-cli');
		assert.equal(Object.hasOwn(staticSiteImporterRecipe.workflow.steps[0], 'metadata'), false);
		assert.deepEqual(staticSiteImporterRecipe.workflow.steps[0].args, ['command=plugin activate static-site-importer/static-site-importer.php']);
		assert.equal(staticSiteImporterRecipe.workflow.steps[1].command, 'wordpress.wp-cli');

		assert.equal(classifyStaticSiteFinding({ message: 'This block contains unexpected or invalid content' }).group_key, 'invalid_block_content');
		assert.equal(classifyStaticSiteFinding({ code: 'runtime_dependency_target_missing', message: '#canvas' }).group_key, 'runtime_target_gap');
		assert.equal(classifyStaticSiteFinding({ message: 'The imported page has default gray buttons' }).group_key, 'button_style_loss');

		const result = normalizeStaticSiteFixtureMatrixResult({
			matrix,
			results: [
				{
					fixture_id: 'saveweb2zip-com-liquidbonsai-com',
					status: 'failed',
					diagnostics: [
						{ code: 'runtime_dependency_target_missing', message: 'Missing target #canvas' },
					],
				},
				{
					fixture_id: '41-generative-art-studio',
					status: 'failed',
					diagnostics: [
						{ message: 'This block contains unexpected or invalid content' },
						{ message: 'Dropped image asset missing.svg' },
					],
				},
			],
		});
		assert.equal(result.schema, 'homeboy/static-site-fixture-matrix-result/v1');
		assert.equal(result.summary.failed, 2);
		assert.equal(result.summary.finding_count, 3);
		assert.equal(result.summary.groups.runtime_target_gap, 1);
		assert.equal(result.summary.groups.invalid_block_content, 1);
		assert.equal(result.summary.groups.dropped_images, 1);
		assert.deepEqual(result.fanout_groups.map((group) => group.key).sort(), [
			'dropped_images',
			'invalid_block_content',
			'runtime_target_gap',
		]);

		const outputDirectory = path.join(root, 'artifacts');
		const written = writeStaticSiteFixtureMatrixArtifacts({ outputDirectory, matrix, result });
		assert.equal(written.artifact_refs.length, 4);
		assert.equal(readJson(path.join(outputDirectory, 'summary.json')).finding_count, 3);
		assert.equal(readJson(path.join(outputDirectory, '41-generative-art-studio', 'artifact.json')).entry_path, 'website/index.html');

		write(
			path.join(outputDirectory, '41-generative-art-studio', 'validation-result.json'),
			JSON.stringify({
				fixture_id: '41-generative-art-studio',
				success: false,
				ssi_validation: { valid: false },
				import_report: { report: { quality: { invalid_block_count: 2, fallback_count: 1 } } },
				missing_assets: [{ path: 'missing.svg', message: 'SVG asset missing.svg was not materialized' }],
				artifacts: { import_report: '41-generative-art-studio/import-report.json' },
			})
		);
		write(
			path.join(outputDirectory, 'saveweb2zip-com-liquidbonsai-com', 'validation-result.json'),
			JSON.stringify({
				fixture_id: 'saveweb2zip-com-liquidbonsai-com',
				success: false,
				runtime_target_gaps: [{ selector: '#canvas', code: 'runtime_dependency_target_missing' }],
			})
		);
		const collected = collectStaticSiteFixtureMatrixRunResults({
			matrix,
			outputDirectory,
			codeboxOutput: {
				ok: false,
				steps: [
					{
						metadata: { fixture_id: 'saveweb2zip-com-liquidbonsai-com' },
						status: 'failed',
						stdout: JSON.stringify({
							status: 'failed',
							blocks_engine: {
								diagnostics: [{ code: 'runtime_dependency_target_missing', message: 'Missing #canvas target' }],
							},
						}),
					},
				],
			},
		});
		assert.equal(collected.summary.failed, 2);
		assert.equal(collected.summary.groups.runtime_target_gap, 2);
		assert.equal(collected.summary.groups.invalid_block_content, 1);
		assert.equal(collected.summary.groups.broken_svg, 1);
		const studioResult = collected.fixtures.find((fixture) => fixture.fixture_id === '41-generative-art-studio');
		assert.deepEqual(studioResult.invalid_block_counts, { invalid_block_count: 2 });
		assert.equal(studioResult.import_report.report.quality.fallback_count, 1);
		assert.equal(studioResult.missing_assets[0].path, 'missing.svg');
		assert.ok(studioResult.artifact_refs.some((ref) => ref.path.endsWith('validation-result.json')));

		const partialDirectory = path.join(root, 'partial-artifacts');
		const partialMatrix = createStaticSiteFixtureMatrix({
			id: 'partial-run',
			fixtures: matrix.fixtures,
		});
		writeStaticSiteFixtureMatrixArtifacts({ outputDirectory: partialDirectory, matrix: partialMatrix });
		write(
			path.join(partialDirectory, '41-generative-art-studio', 'validation-result.json'),
			JSON.stringify({ fixture_id: '41-generative-art-studio', success: true, diagnostics: [] })
		);
		const partial = collectStaticSiteFixtureMatrixRunResults({
			matrix: partialMatrix,
			outputDirectory: partialDirectory,
			codeboxError: new Error('WP Codebox stopped after fixture failure'),
		});
		writeStaticSiteFixtureMatrixResultArtifacts({ outputDirectory: partialDirectory, matrix: partialMatrix, result: partial });
		assert.equal(partial.summary.succeeded, 1);
		assert.equal(partial.summary.failed, 1);
		assert.equal(readJson(path.join(partialDirectory, 'summary.json')).failed, 1);
		assert.ok(partial.findings.some((finding) => finding.fixture_id === 'saveweb2zip-com-liquidbonsai-com'));

		console.log('static-site fixture matrix smoke passed');
	} finally {
		fs.rmSync(root, { recursive: true, force: true });
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
