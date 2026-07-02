#!/usr/bin/env node
/**
 * External dependencies
 */
import { readFileSync, writeFileSync } from 'node:fs';

/**
 * Internal dependencies
 */
import { loadCodeboxRecipeBuilder } from '../bench/wp-codebox-recipe-builder-loader.mjs';
import { applyWpCodeboxStepDiagnostics } from '../lib/wp-codebox-diagnostics-plan.mjs';

const input = JSON.parse(readFileSync(0, 'utf8'));
const { builder: buildWordPressPhpunitRecipe, source } = await loadCodeboxRecipeBuilder('buildWordPressPhpunitRecipe');
const options = input.options ?? input;
const recipe = buildWordPressPhpunitRecipe(options);
applyWpCodeboxStepDiagnostics(recipe, options);

if (process.env.HOMEBOY_WP_CODEBOX_RECIPE_BUILDER_SOURCE_FILE) {
	writeFileSync(process.env.HOMEBOY_WP_CODEBOX_RECIPE_BUILDER_SOURCE_FILE, `${source}\n`);
}

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);
