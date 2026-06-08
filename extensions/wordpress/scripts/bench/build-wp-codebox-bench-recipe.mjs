#!/usr/bin/env node
/**
 * External dependencies
 */
import { readFileSync } from 'node:fs';

/**
 * Internal dependencies
 */
import { loadCodeboxRecipeBuilder } from './wp-codebox-recipe-builder-loader.mjs';

const input = JSON.parse(readFileSync(0, 'utf8'));
const { builder: buildWordPressBenchRecipe } = await loadCodeboxRecipeBuilder('buildWordPressBenchRecipe');
const options = { ...(input.options || {}) };
if (!options.extra_plugins && options.extraPlugins) {
	options.extra_plugins = options.extraPlugins;
}

const recipe = buildWordPressBenchRecipe(options);
if (options.pluginRuntime && typeof options.pluginRuntime === 'object' && !Array.isArray(options.pluginRuntime)) {
	recipe.inputs = recipe.inputs ?? {};
	recipe.inputs.pluginRuntime = options.pluginRuntime;
}

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);
