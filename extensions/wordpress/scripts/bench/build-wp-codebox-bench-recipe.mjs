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

const recipe = buildWordPressBenchRecipe(input.options);
if (input.options?.pluginRuntime && typeof input.options.pluginRuntime === 'object' && !Array.isArray(input.options.pluginRuntime)) {
	recipe.inputs = recipe.inputs ?? {};
	recipe.inputs.pluginRuntime = input.options.pluginRuntime;
}

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);
