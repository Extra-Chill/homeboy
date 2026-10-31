'use strict';

/* eslint-disable no-console */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	FIXTURE_ARTIFACT_SCHEMA,
	buildFixtureArtifact,
	buildWpCodeboxFixtureWorkloadMatrixRecipe,
	collectFixtureWorkloadMatrixRunResults,
	createFixtureWorkloadMatrix,
	discoverFixtureWorkloads,
	normalizeFixtureWorkloadMatrixResult,
	writeFixtureWorkloadMatrixArtifacts,
	writeFixtureWorkloadMatrixResultArtifacts,
} = require('../lib/fixture-workload-matrix');

function write(filePath, contents) {
	fs.mkdirSync(path.dirname(filePath), { recursive: true });
	fs.writeFileSync(filePath, contents);
}

function readJson(filePath) {
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

async function main() {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-fixture-workload-matrix-'));
	try {
		const alpha = path.join(root, 'alpha-case');
		const beta = path.join(root, 'beta-case');
		write(path.join(alpha, 'index.html'), '<main>Alpha</main>');
		write(path.join(alpha, 'data.json'), '{"fixture":"alpha"}');
		write(path.join(beta, 'index.html'), '<main>Beta</main>');
		write(path.join(beta, 'asset.bin'), Buffer.from([1, 2, 3]));

		const fixtures = discoverFixtureWorkloads(root);
		assert.deepEqual(fixtures.map((fixture) => fixture.id), ['alpha-case', 'beta-case']);

		const matrix = createFixtureWorkloadMatrix({ fixture_root: root, id: 'fixture-run', batchSize: 1 });
		assert.equal(matrix.schema, 'homeboy/fixture-workload-matrix/v1');
		assert.equal(matrix.count, 2);
		assert.equal(matrix.batch_count, 2);
		assert.deepEqual(matrix.batches.map((batch) => batch.fixture_ids), [['alpha-case'], ['beta-case']]);

		const artifact = buildFixtureArtifact(matrix.fixtures.find((fixture) => fixture.id === 'alpha-case'));
		assert.equal(artifact.schema, FIXTURE_ARTIFACT_SCHEMA);
		assert.equal(artifact.entrypoint, 'fixture/index.html');
		assert.ok(artifact.files.some((file) => file.path === 'fixture/index.html'));
		assert.ok(artifact.files.some((file) => file.path === 'fixture/data.json'));

		const recipe = buildWpCodeboxFixtureWorkloadMatrixRecipe({
			matrix,
			artifactsDirectory: '/host/artifacts/matrix',
			playgroundArtifactsDirectory: '/wordpress/wp-content/uploads/fixture-workload-matrix',
			extra_plugins: [{ source: '/workspace/example-plugin', slug: 'example-plugin', activate: false }],
			pluginActivations: ['example-plugin/example-plugin.php'],
			workloadStep: {
				command: 'wordpress.wp-cli',
				args: ['command=example validate --artifact={{ artifact_path }} --fixture={{ fixture_id }}'],
			},
		});
		assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
		assert.deepEqual(Object.keys(recipe).sort(), ['artifacts', 'inputs', 'runtime', 'schema', 'workflow']);
		assert.deepEqual(recipe.inputs.extra_plugins, [{ source: '/workspace/example-plugin', slug: 'example-plugin', activate: false }]);
		assert.equal(Object.hasOwn(recipe.inputs, 'extraPlugins'), false);
		assert.deepEqual(recipe.workflow.steps[0].args, ['command=plugin activate example-plugin/example-plugin.php']);
		assert.match(recipe.workflow.steps[1].args[0], /command=example validate/);
		assert.match(recipe.workflow.steps[1].args[0], /--artifact=\/wordpress\/wp-content\/uploads\/fixture-workload-matrix\/alpha-case\/artifact.json/);
		assert.match(recipe.workflow.steps[2].args[0], /--fixture=beta-case/);

		const result = normalizeFixtureWorkloadMatrixResult({
			matrix,
			results: [
				{ fixture_id: 'alpha-case', status: 'passed', diagnostics: [] },
				{ fixture_id: 'beta-case', status: 'failed', diagnostics: [{ code: 'opaque_problem', message: 'Caller-owned diagnostic packet.' }] },
			],
			compareFixtureResult: (fixtureResult) => ({ comparable: fixtureResult.fixture_id }),
		});
		assert.equal(result.schema, 'homeboy/fixture-workload-matrix-result/v1');
		assert.equal(result.summary.succeeded, 1);
		assert.equal(result.summary.failed, 1);
		assert.equal(result.summary.diagnostic_count, 1);
		assert.equal(result.summary.groups.opaque_problem, 1);
		assert.equal(result.fixtures[1].comparison.comparable, 'beta-case');

		const actionable = normalizeFixtureWorkloadMatrixResult({
			matrix,
			summaryLimit: 1,
			results: [
				{ fixture_id: 'alpha-case', status: 'passed', diagnostics: [] },
				{
					fixture_id: 'beta-case',
					status: 'failed',
					artifact_refs: [{ artifact_id: 'visual-diff', url: 'https://artifacts.example.test/beta/visual-diff.json' }],
					diagnostics: [{
						kind: 'visual_mismatch',
						category: 'visual_comparison',
						severity: 'error',
						reason: 'Pixel difference exceeded the permitted threshold by 6 pixels.',
						selector: 'main .hero',
						block_name: 'core/html',
						element: 'img',
						retryable: true,
						artifact_refs: [{ artifact_id: 'comparison', href: 'https://artifacts.example.test/beta/comparison.html' }],
					}],
				},
			],
		});
		assert.deepEqual(actionable.summary.failure_summaries, [{
			fixture_id: 'beta-case',
			status: 'failed',
			kind: 'visual_mismatch',
			category: 'visual_comparison',
			severity: 'error',
			reason: 'Pixel difference exceeded the permitted threshold by 6 pixels.',
			artifact_refs: [
				{ artifact_id: 'comparison', href: 'https://artifacts.example.test/beta/comparison.html' },
				{ artifact_id: 'visual-diff', url: 'https://artifacts.example.test/beta/visual-diff.json' },
			],
			retryable: true,
		}]);
		assert.deepEqual(actionable.summary.top_diagnostic_kinds, [{ value: 'visual_mismatch', count: 1 }]);
		assert.deepEqual(actionable.summary.top_fixtures_by_finding_count, [{ fixture_id: 'beta-case', finding_count: 1 }]);
		assert.deepEqual(actionable.summary.top_severities, [{ value: 'error', count: 1 }]);
		assert.deepEqual(actionable.summary.top_categories, [{ value: 'visual_comparison', count: 1 }]);
		assert.deepEqual(actionable.summary.top_runtime_target_selectors, [{ value: 'main .hero', count: 1 }]);
		assert.deepEqual(actionable.summary.top_core_html_sources, [{ value: 'img', count: 1 }]);

		const deduped = normalizeFixtureWorkloadMatrixResult({
			matrix,
			results: [
				{
					fixture_id: 'alpha-case',
					status: 'failed',
					diagnostics: [
						{
							id: 'specific-packet',
							kind: 'specific_rule',
							category: 'specific_owner',
							group_key: 'specific_owner',
							severity: 'warning',
							path: 'fixture/index.html',
							source_path: 'fixture/index.html',
							selector: '.target',
							reason: 'same underlying source issue',
							repair_mode: 'source-repair',
						},
						{
							id: 'wrapper-packet',
							kind: 'same-underlying-source-issue',
							category: 'wrapper_owner',
							group_key: 'wrapper_owner',
							severity: 'warning',
							path: 'fixture/index.html',
							source_path: 'fixture/index.html',
							selector: '.target',
							reason: 'same underlying source issue',
						},
					],
				},
				{ fixture_id: 'beta-case', status: 'passed', diagnostics: [] },
			],
		});
		assert.equal(deduped.summary.diagnostic_count, 1);
		assert.equal(deduped.summary.groups.specific_owner, 1);
		assert.equal(deduped.diagnostics[0].id, 'specific-packet');
		assert.deepEqual(deduped.diagnostics[0].duplicate_diagnostic_ids, ['wrapper-packet']);

		const outputDirectory = path.join(root, 'artifacts');
		const written = writeFixtureWorkloadMatrixArtifacts({ outputDirectory, matrix, result });
		assert.equal(written.artifact_refs.length, 4);
		assert.equal(readJson(path.join(outputDirectory, 'summary.json')).diagnostic_count, 1);
		assert.equal(readJson(path.join(outputDirectory, 'alpha-case', 'artifact.json')).entry_path, 'fixture/index.html');

		write(path.join(outputDirectory, 'beta-case', 'result.json'), JSON.stringify({ fixture_id: 'beta-case', success: false, diagnostics: [{ category: 'caller_bucket', message: 'Opaque failure.' }] }));
		const collected = collectFixtureWorkloadMatrixRunResults({
			matrix,
			outputDirectory,
			codeboxOutput: {
				executions: [{ metadata: { fixture_id: 'alpha-case' }, stdout: JSON.stringify({ success: true, artifacts: { report: 'alpha-case/report.json' } }) }],
			},
		});
		assert.equal(collected.summary.succeeded, 1);
		assert.equal(collected.summary.failed, 1);
		assert.equal(collected.summary.groups.caller_bucket, 1);
		assert.ok(collected.fixtures.find((fixture) => fixture.fixture_id === 'alpha-case').artifact_refs.some((ref) => ref.artifact_id === 'report'));

		const partialDirectory = path.join(root, 'partial-artifacts');
		writeFixtureWorkloadMatrixArtifacts({ outputDirectory: partialDirectory, matrix });
		write(path.join(partialDirectory, 'alpha-case', 'result.json'), JSON.stringify({ fixture_id: 'alpha-case', success: true, diagnostics: [] }));
		const partial = collectFixtureWorkloadMatrixRunResults({ matrix, outputDirectory: partialDirectory, codeboxError: new Error('runtime stopped') });
		writeFixtureWorkloadMatrixResultArtifacts({ outputDirectory: partialDirectory, matrix, result: partial });
		assert.equal(partial.summary.succeeded, 1);
		assert.equal(partial.summary.failed, 1);
		assert.ok(partial.diagnostics.some((diagnostic) => diagnostic.fixture_id === 'beta-case'));

		const sourceText = fs.readFileSync(path.join(__dirname, '..', 'lib', 'fixture-workload-matrix.js'), 'utf8');
		assert.doesNotMatch(sourceText, /Static Site|static-site|static_site|\bssi\b|\bSSI\b|parser repair|importer|invalid_block|dropped_images|runtime_target_gap/);

		console.log('fixture workload matrix smoke passed');
	} finally {
		fs.rmSync(root, { recursive: true, force: true });
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
