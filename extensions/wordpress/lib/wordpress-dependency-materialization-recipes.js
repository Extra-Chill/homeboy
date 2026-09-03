'use strict';

const WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES_SCHEMA = 'homeboy-extension-wordpress/dependency-materialization-recipes/v1';
const WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS_SCHEMA = 'homeboy-extension-wordpress/required-output-declarations/v1';

// The extension manifest is the public source of recipe data.
const WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES = require('../wordpress.json').dependency_materialization_recipes;

const WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS = Object.freeze({
	schema: WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS_SCHEMA,
	declaration_shape: Object.freeze({
		id: 'string',
		kind: 'file|directory|glob',
		path: 'component-relative path or glob',
		producer: 'dependency-materialization|build|runtime-prepare',
		required: 'boolean',
	}),
	examples: Object.freeze([
		Object.freeze({
			id: 'built-php-autoloader',
			kind: 'file',
			path: 'vendor/autoload.php',
			producer: 'dependency-materialization',
			required: true,
		}),
		Object.freeze({
			id: 'built-js-assets',
			kind: 'glob',
			path: 'build/**/*',
			producer: 'build',
			required: true,
		}),
	]),
});

module.exports = {
	WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES_SCHEMA,
	WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES,
	WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS_SCHEMA,
	WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS,
};
