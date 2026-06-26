const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	coreModuleCandidates,
	loadWpCodeboxCoreExport,
} = require('../lib/wp-codebox-core-loader');

async function main() {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-core-loader-'));
	try {
		const dist = path.join(root, 'source', 'node_modules', '@automattic', 'wp-codebox-core', 'dist');
		fs.mkdirSync(dist, { recursive: true });
		fs.writeFileSync(path.join(dist, 'artifacts.js'), 'exports.fixtureExport = () => "focused-artifacts";\n');
		fs.writeFileSync(path.join(dist, 'index.js'), 'exports.fixtureExport = () => "legacy-index";\n');

		const candidates = coreModuleCandidates({
			packageCandidates: [
				'@automattic/wp-codebox-core/artifacts',
				'wp-codebox-workspace/artifacts',
			],
			includeGlobalNodeModuleRoots: false,
		});

		assert.equal(candidates[0], '@automattic/wp-codebox-core/artifacts');
		assert.equal(candidates[1], 'wp-codebox-workspace/artifacts');
		assert.equal(candidates.includes('@automattic/wp-codebox-core/artifacts'), true);

		const result = await loadWpCodeboxCoreExport('fixtureExport', {
			coreModule: path.join(dist, 'artifacts.js'),
			required: true,
		});

		assert.match(result.source, /dist\/artifacts\.js$/);
		assert.equal(result.value(), 'focused-artifacts');

		const missingExportPath = path.join(dist, 'missing-artifacts.js');
		await assert.rejects(() => loadWpCodeboxCoreExport('fixtureExport', {
			coreModule: missingExportPath,
			required: true,
		}), /WP Codebox core export fixtureExport is unavailable/);

		const globalRoot = path.join(root, 'global-node-modules');
		const globalRecipeBuilders = path.join(globalRoot, 'wp-codebox-workspace', 'packages', 'runtime-core', 'dist');
		fs.mkdirSync(globalRecipeBuilders, { recursive: true });
		fs.writeFileSync(path.join(globalRecipeBuilders, 'recipe-builders.js'), 'exports.fixtureExport = () => "global-runtime-core";\n');

		const globalCandidates = coreModuleCandidates({
			packageCandidates: [],
			globalNodeModuleRoots: [globalRoot],
			runtimeCoreEntries: ['packages/runtime-core/dist/recipe-builders.js'],
			packageDistEntries: ['recipe-builders.js'],
		});
		assert.ok(globalCandidates.some((candidate) => candidate.endsWith('/wp-codebox-workspace/packages/runtime-core/dist/recipe-builders.js')));

		const globalResult = await loadWpCodeboxCoreExport('fixtureExport', {
			packageCandidates: [],
			globalNodeModuleRoots: [globalRoot],
			runtimeCoreEntries: ['packages/runtime-core/dist/recipe-builders.js'],
			packageDistEntries: ['recipe-builders.js'],
			required: true,
		});
		assert.equal(globalResult.value(), 'global-runtime-core');

		console.log('wp-codebox core loader smoke passed');
	} finally {
		fs.rmSync(root, { recursive: true, force: true });
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
