'use strict';

// Pins the extension's artifact-cleanup declarations to what its own
// bootstrap/install behavior actually produces. A declaration that drifts from
// the materialization recipes would either leave reclaimable trees invisible to
// `homeboy cleanup artifacts` or offer rehydration guidance that cannot run.

const assert = require('node:assert/strict');

const wordpressManifest = require('../wordpress.json');

const RECONSTRUCTABLE_CATEGORIES = new Set(['dependencies', 'build_output', 'build_cache']);
const KNOWN_CATEGORIES = new Set([...RECONSTRUCTABLE_CATEGORIES, 'release_asset']);
const DECLARATION_KEYS = new Set([
	'id',
	'category',
	'path',
	'scopes',
	'rehydrate_command',
	'min_age_days',
	'description',
]);
const SCOPE_KEYS = new Set(['manifest_files', 'nested', 'max_depth']);

const config = wordpressManifest.artifact_cleanup;
assert.ok(config, 'wordpress.json declares artifact_cleanup');
assert.deepEqual(Object.keys(config), ['declarations'], 'artifact_cleanup carries only declarations');

const declarations = config.declarations;
const byId = new Map(declarations.map((declaration) => [declaration.id, declaration]));

assert.deepEqual(
	declarations.map((declaration) => declaration.id),
	[
		'npm-node-modules',
		'composer-vendor',
		'generated-js-assets',
		'phpunit-result-cache',
		'packaged-release-output',
	],
	'declaration set is pinned'
);
assert.equal(byId.size, declarations.length, 'declaration ids are unique');

for (const declaration of declarations) {
	const label = `declaration ${declaration.id}`;

	for (const key of Object.keys(declaration)) {
		assert.ok(DECLARATION_KEYS.has(key), `${label} uses only contract keys (saw ${key})`);
	}
	assert.ok(KNOWN_CATEGORIES.has(declaration.category), `${label} declares a known category`);
	assert.ok(declaration.description, `${label} states its retention/readiness tradeoff`);

	// Containment is enforced by Homeboy, but a declaration that could never be
	// accepted is a manifest bug worth catching here rather than at cleanup time.
	assert.ok(
		declaration.path && !declaration.path.startsWith('/') && !declaration.path.split('/').includes('..'),
		`${label} declares a contained relative path`
	);

	assert.ok(Array.isArray(declaration.scopes) && declaration.scopes.length > 0, `${label} declares install scopes`);
	for (const scope of declaration.scopes) {
		for (const key of Object.keys(scope)) {
			assert.ok(SCOPE_KEYS.has(key), `${label} scope uses only contract keys (saw ${key})`);
		}
		assert.ok(
			Array.isArray(scope.manifest_files) && scope.manifest_files.length > 0,
			`${label} anchors each scope to a manifest this extension supports`
		);
		assert.ok(
			typeof scope.max_depth === 'number' && scope.max_depth > 0,
			`${label} bounds nested scope discovery`
		);
	}

	if (RECONSTRUCTABLE_CATEGORIES.has(declaration.category) && declaration.category !== 'build_cache') {
		assert.ok(declaration.rehydrate_command, `${label} reports how to rehydrate what cleanup removes`);
	}
}

// Reconstructable dependency trees must match the materialization recipes: the
// same manifest/lockfile pair anchors the scope, the same directory is the
// artifact, and the rehydration command is the one that installs it.
const recipes = wordpressManifest.dependency_materialization_recipes.recipes;
const recipeByPath = new Map(
	recipes.map((recipe) => [recipe.materializes[0].path, recipe])
);

for (const id of ['npm-node-modules', 'composer-vendor']) {
	const declaration = byId.get(id);
	const recipe = recipeByPath.get(declaration.path);

	assert.ok(recipe, `${id} cleans a directory this extension materializes`);
	assert.equal(declaration.category, 'dependencies', `${id} is a dependency tree`);
	assert.deepEqual(
		declaration.scopes[0].manifest_files,
		[recipe.declaration.manifest, recipe.declaration.lockfile],
		`${id} resolves only where its manifest and lockfile pair exists`
	);
	assert.equal(declaration.scopes[0].nested, true, `${id} resolves nested install scopes`);
	assert.ok(declaration.min_age_days >= 1, `${id} keeps a freshly installed tree`);
}

assert.equal(byId.get('npm-node-modules').rehydrate_command, 'npm ci');
assert.equal(
	byId.get('composer-vendor').rehydrate_command,
	'composer install --no-interaction --prefer-dist'
);

// Packaging output holds the deployable this extension's artifact_pattern points
// at, so it stays inventory-only.
const packaged = byId.get('packaged-release-output');
assert.equal(packaged.category, 'release_asset', 'packaged output is never reclaimed');
assert.equal(packaged.rehydrate_command, undefined, 'a retained asset needs no rehydration guidance');
assert.ok(
	wordpressManifest.build.artifact_pattern.startsWith(`${packaged.path}/`),
	'the retained path is where the build artifact is written'
);

// Post-deploy cleanup and worktree artifact cleanup describe the same trees;
// a path in one that no ecosystem produces is dead configuration.
const materializedPaths = new Set(recipes.map((recipe) => recipe.materializes[0].path));
for (const path of wordpressManifest.build.cleanup_paths) {
	assert.ok(
		materializedPaths.has(path) || path === packaged.path,
		`post-deploy cleanup path ${path} is produced by this extension`
	);
}

console.log('wordpress artifact cleanup declarations smoke passed');
