import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const manifestRoot = path.join(repoRoot, 'dependency-adapters');
const schema = JSON.parse(fs.readFileSync(path.join(manifestRoot, 'schema.json'), 'utf8'));
const index = JSON.parse(fs.readFileSync(path.join(manifestRoot, 'index.json'), 'utf8'));
const exampleDir = path.join(manifestRoot, 'examples');
const exampleFiles = fs.readdirSync(exampleDir).filter((file) => file.endsWith('.json')).sort();

assert.equal(schema.properties.schema.const, 'homeboy-extension/dependency-adapter-manifest/v1');
assert.ok(schema.required.includes('ecosystem'));
assert.ok(schema.required.includes('project_signals'));
assert.ok(schema.required.includes('capabilities'));
assert.deepEqual(exampleFiles, ['composer.json', 'nodejs.json', 'wordpress.json']);
assert.equal(index.schema, 'homeboy-extension/dependency-adapter-index/v1');
assert.deepEqual(
	index.manifests.map((manifest) => manifest.path).sort(),
	exampleFiles.map((file) => `examples/${file}`).sort(),
	'index lists every shipped adapter manifest'
);

const ids = new Set();
const productSpecificTerms = ['woocommerce', 'jetpack', 'studio-native'];

for (const file of exampleFiles) {
	const manifest = JSON.parse(fs.readFileSync(path.join(exampleDir, file), 'utf8'));
	const indexEntry = index.manifests.find((entry) => entry.path === `examples/${file}`);
	const serialized = JSON.stringify(manifest).toLowerCase();

	assert.ok(indexEntry, `${file} has an index entry`);
	assert.equal(indexEntry.id, manifest.id, `${file} index id matches manifest id`);
	assert.equal(indexEntry.ecosystem, manifest.ecosystem, `${file} index ecosystem matches manifest ecosystem`);
	assert.equal(manifest.schema, schema.properties.schema.const, `${file} uses the adapter schema`);
	assert.equal(typeof manifest.id, 'string', `${file} has an id`);
	assert.equal(typeof manifest.version, 'number', `${file} has a version`);
	assert.equal(typeof manifest.ecosystem, 'string', `${file} has an ecosystem`);
	assert.ok(Array.isArray(manifest.project_signals.root_files), `${file} declares root files`);
	assert.ok(manifest.project_signals.root_files.length > 0, `${file} declares at least one root signal`);
	assert.equal(typeof manifest.capabilities, 'object', `${file} declares capabilities`);
	assert.ok(!ids.has(manifest.id), `${manifest.id} is unique`);
	ids.add(manifest.id);

	for (const term of productSpecificTerms) {
		assert.equal(serialized.includes(term), false, `${file} stays product-neutral: ${term}`);
	}

	for (const manager of manifest.package_managers || []) {
		assert.equal(typeof manager.id, 'string', `${file} package manager has id`);
		assert.equal(typeof manager.selection.priority, 'number', `${manager.id} has priority`);
		assert.equal(typeof manager.install.intent, 'string', `${manager.id} has install intent`);
		assert.equal(typeof manager.commands?.status?.command, 'string', `${manager.id} declares a status command`);
		assert.equal(typeof manager.commands?.install?.command, 'string', `${manager.id} declares an install command`);
		assert.equal(typeof manager.commands?.update?.command, 'string', `${manager.id} declares an update command`);
		assert.equal(typeof manager.package_identity?.manifest, 'string', `${manager.id} declares package identity manifest`);
		assert.equal(typeof manager.package_identity?.name, 'string', `${manager.id} declares package name path`);
		assert.equal(typeof manager.package_identity?.version, 'string', `${manager.id} declares package version path`);
		assert.ok(Array.isArray(manager.package_identity.dependencies), `${manager.id} declares dependency paths`);
		assert.ok(Array.isArray(manager.outputs), `${manager.id} declares outputs`);
		assert.ok(manager.outputs.length > 0, `${manager.id} has at least one output`);
	}

	for (const helper of manifest.helpers || []) {
		assert.equal(typeof helper.id, 'string', `${file} helper has id`);
		assert.ok(Array.isArray(helper.requires), `${helper.id} declares requirements`);
		assert.ok(Array.isArray(helper.outputs), `${helper.id} declares outputs`);
	}
}

const nodeManifest = JSON.parse(fs.readFileSync(path.join(exampleDir, 'nodejs.json'), 'utf8'));
assert.deepEqual(nodeManifest.lockfile_priority, ['pnpm-lock.yaml', 'yarn.lock', 'package-lock.json']);
assert.deepEqual(
	nodeManifest.package_managers.map((manager) => manager.id),
	['pnpm', 'yarn', 'npm'],
	'Node.js adapter documents the current deterministic package-manager priority'
);

const composerManifest = JSON.parse(fs.readFileSync(path.join(exampleDir, 'composer.json'), 'utf8'));
assert.deepEqual(composerManifest.lockfile_priority, ['composer.lock']);

const adapterModule = await import(path.join(manifestRoot, 'index.mjs'));
assert.deepEqual(
	adapterModule.dependencyAdapterManifestPaths().map((manifestPath) => path.relative(manifestRoot, manifestPath)).sort(),
	exampleFiles.map((file) => path.join('examples', file)).sort(),
	'index helper returns every adapter manifest path'
);
assert.deepEqual(
	adapterModule.loadDependencyAdapterManifests().map((manifest) => manifest.id).sort(),
	index.manifests.map((manifest) => manifest.id).sort(),
	'index helper loads every adapter manifest'
);

console.log('dependency adapter manifest smoke passed');
