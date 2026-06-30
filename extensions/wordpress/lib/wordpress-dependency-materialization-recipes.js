'use strict';

const WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES_SCHEMA = 'homeboy-extension-wordpress/dependency-materialization-recipes/v1';
const WORDPRESS_REQUIRED_OUTPUT_DECLARATIONS_SCHEMA = 'homeboy-extension-wordpress/required-output-declarations/v1';

const WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES = Object.freeze({
	schema: WORDPRESS_DEPENDENCY_MATERIALIZATION_RECIPES_SCHEMA,
	recipes: Object.freeze([
		Object.freeze({
			id: 'wordpress-php-package-dependencies',
			kind: 'php-package-dependency-materialization',
			ecosystem: 'php',
			package_manager: 'composer',
			declaration: Object.freeze({
				manifest: 'composer.json',
				lockfile: 'composer.lock',
			}),
			materializes: Object.freeze([
				Object.freeze({ kind: 'directory', path: 'vendor' }),
			]),
			required_outputs: Object.freeze([
				Object.freeze({ id: 'composer-vendor-directory', kind: 'directory', path: 'vendor' }),
			]),
		}),
		Object.freeze({
			id: 'wordpress-js-asset-dependencies',
			kind: 'js-asset-dependency-materialization',
			ecosystem: 'javascript',
			package_manager: 'npm',
			declaration: Object.freeze({
				manifest: 'package.json',
				lockfile: 'package-lock.json',
			}),
			materializes: Object.freeze([
				Object.freeze({ kind: 'directory', path: 'node_modules' }),
			]),
			required_outputs: Object.freeze([
				Object.freeze({ id: 'npm-node-modules-directory', kind: 'directory', path: 'node_modules' }),
			]),
		}),
	]),
});

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
