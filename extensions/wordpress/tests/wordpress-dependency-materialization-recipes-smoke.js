'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES,
	WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES_SCHEMA,
	WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS,
	WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS_SCHEMA,
} = require('../lib/wordpress-dependency-materialization-recipes');
const wordpressManifest = require('../wordpress.json');

assert.equal(WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES.schema, WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES_SCHEMA);
assert.deepEqual(WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES.recipes.map((recipe) => recipe.id), [
	'wordpress-php-package-dependencies',
	'wordpress-js-asset-dependencies',
]);

const phpRecipe = WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES.recipes[0];
assert.equal(phpRecipe.kind, 'php-package-dependency-materialization');
assert.equal(phpRecipe.package_manager, 'composer');
assert.deepEqual(phpRecipe.declaration, { manifest: 'composer.json', lockfile: 'composer.lock' });
assert.deepEqual(phpRecipe.required_outputs, [
	{ id: 'composer-vendor-directory', kind: 'directory', path: 'vendor' },
]);

const jsRecipe = WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES.recipes[1];
assert.equal(jsRecipe.kind, 'js-asset-dependency-materialization');
assert.equal(jsRecipe.package_manager, 'npm');
assert.deepEqual(jsRecipe.declaration, { manifest: 'package.json', lockfile: 'package-lock.json' });
assert.deepEqual(jsRecipe.required_outputs, [
	{ id: 'npm-node-modules-directory', kind: 'directory', path: 'node_modules' },
]);

assert.equal(WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS.schema, WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS_SCHEMA);
assert.deepEqual(Object.keys(WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS.declaration_shape), [
	'id',
	'kind',
	'path',
	'producer',
	'required',
]);
assert.equal(WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS.examples[0].path, 'vendor/autoload.php');
assert.equal(WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS.examples[1].kind, 'glob');
// The manifest copy of the recipes is consumed by
// wordpress-artifact-cleanup-declarations-smoke.js, which cross-checks it
// against the live `artifact_cleanup` declarations, so the two must agree.
assert.deepEqual(wordpressManifest.dependency_materialization_recipes, WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES);

console.log('wordpress dependency materialization recipes smoke passed');
