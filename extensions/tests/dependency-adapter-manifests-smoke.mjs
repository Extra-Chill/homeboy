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

function schemaAt(ref) {
	assert.ok(ref.startsWith('#/'), `Unsupported schema ref: ${ref}`);
	return ref.slice(2).split('/').reduce((current, segment) => current[segment], schema);
}

function validateAgainstSchema(definition, value, label) {
	if (definition.$ref) {
		validateAgainstSchema(schemaAt(definition.$ref), value, label);
		return;
	}

	if (definition.const !== undefined) {
		assert.equal(value, definition.const, `${label} matches const ${definition.const}`);
	}

	if (definition.type) {
		const actualType = Array.isArray(value) ? 'array' : value === null ? 'null' : Number.isInteger(value) ? 'integer' : typeof value;
		if (definition.type === 'integer') {
			assert.equal(actualType, 'integer', `${label} is an integer`);
		} else {
			assert.equal(actualType, definition.type, `${label} is a ${definition.type}`);
		}
	}

	if (definition.enum) {
		assert.ok(definition.enum.includes(value), `${label} is one of ${definition.enum.join(', ')}`);
	}

	if (definition.pattern && typeof value === 'string') {
		assert.match(value, new RegExp(definition.pattern), `${label} matches ${definition.pattern}`);
	}

	if (definition.minimum !== undefined && typeof value === 'number') {
		assert.ok(value >= definition.minimum, `${label} is at least ${definition.minimum}`);
	}

	if (definition.type === 'array') {
		if (definition.minItems !== undefined) {
			assert.ok(value.length >= definition.minItems, `${label} has at least ${definition.minItems} items`);
		}
		for (const [index, item] of value.entries()) {
			validateAgainstSchema(definition.items, item, `${label}[${index}]`);
		}
	}

	if (definition.type === 'object') {
		for (const required of definition.required || []) {
			assert.ok(Object.hasOwn(value, required), `${label} declares ${required}`);
		}

		if (definition.additionalProperties === false) {
			const allowed = new Set(Object.keys(definition.properties || {}));
			for (const key of Object.keys(value)) {
				assert.ok(allowed.has(key), `${label} has schema-declared property ${key}`);
			}
		}

		for (const [key, propertySchema] of Object.entries(definition.properties || {})) {
			if (Object.hasOwn(value, key)) {
				validateAgainstSchema(propertySchema, value[key], `${label}.${key}`);
			}
		}
	}
}

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
assert.ok(Array.isArray(index.manifests), 'index declares manifests array');

const ids = new Set();
const productSpecificTerms = ['woocommerce', 'jetpack', 'studio-native'];

for (const file of exampleFiles) {
	const manifest = JSON.parse(fs.readFileSync(path.join(exampleDir, file), 'utf8'));
	const indexEntry = index.manifests.find((entry) => entry.path === `examples/${file}`);
	const serialized = JSON.stringify(manifest).toLowerCase();

	validateAgainstSchema(schema, manifest, file);

	assert.ok(indexEntry, `${file} has an index entry`);
	assert.equal(typeof indexEntry.id, 'string', `${file} index id is a string`);
	assert.equal(typeof indexEntry.ecosystem, 'string', `${file} index ecosystem is a string`);
	assert.equal(typeof indexEntry.path, 'string', `${file} index path is a string`);
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
assert.deepEqual(nodeManifest.lockfile_priority, ['pnpm-lock.yaml', 'yarn.lock', 'npm-shrinkwrap.json', 'package-lock.json']);
assert.deepEqual(
	nodeManifest.package_managers.map((manager) => manager.id),
	['pnpm', 'yarn', 'npm'],
	'Node.js adapter documents the current deterministic package-manager priority'
);
assert.deepEqual(
	nodeManifest.package_managers.find((manager) => manager.id === 'npm').selection.files,
	['npm-shrinkwrap.json', 'package-lock.json'],
	'npm recognizes both authoritative lockfile names'
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
