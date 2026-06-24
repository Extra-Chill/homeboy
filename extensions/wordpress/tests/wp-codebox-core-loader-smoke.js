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
			wpCodeboxInstallDir: root,
			packageCandidates: [
				'@automattic/wp-codebox-core/artifacts',
				'wp-codebox-workspace/artifacts',
			],
			packageDistEntries: ['artifacts.js'],
		});

		assert.equal(candidates[0], '@automattic/wp-codebox-core/artifacts');
		assert.equal(candidates[1], 'wp-codebox-workspace/artifacts');
		assert.match(candidates[2], /dist\/artifacts\.js$/);
		assert.equal(candidates.some((candidate) => /dist\/index\.js$/.test(candidate)), false);

		const result = await loadWpCodeboxCoreExport('fixtureExport', {
			wpCodeboxInstallDir: root,
			packageCandidates: [],
			packageDistEntries: ['artifacts.js'],
			required: true,
		});

		assert.match(result.source, /dist\/artifacts\.js$/);
		assert.equal(result.value(), 'focused-artifacts');

		fs.rmSync(path.join(dist, 'artifacts.js'));
		await assert.rejects(() => loadWpCodeboxCoreExport('fixtureExport', {
			wpCodeboxInstallDir: root,
			packageCandidates: [],
			packageDistEntries: ['artifacts.js'],
			required: true,
		}), /WP Codebox core export fixtureExport is unavailable/);

		console.log('wp-codebox core loader smoke passed');
	} finally {
		fs.rmSync(root, { recursive: true, force: true });
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
