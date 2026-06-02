#!/usr/bin/env node
/**
 * External dependencies
 */
import { resolve } from 'node:path';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

/**
 * Internal dependencies
 */
import { buildWordPressBenchRecipe as buildBundledWordPressBenchRecipe } from './wp-codebox-bench-recipe-builder.mjs';

const input = JSON.parse(readFileSync(0, 'utf8'));
const { buildWordPressBenchRecipe, source } = await loadRecipeBuilder();

if (typeof buildWordPressBenchRecipe !== 'function') {
	throw new Error(`WP Codebox recipe builder source ${source} does not export buildWordPressBenchRecipe().`);
}

const recipe = buildWordPressBenchRecipe(input.options);
if (input.options?.pluginRuntime && typeof input.options.pluginRuntime === 'object' && !Array.isArray(input.options.pluginRuntime)) {
	recipe.inputs = recipe.inputs ?? {};
	recipe.inputs.pluginRuntime = input.options.pluginRuntime;
}

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);

async function loadRecipeBuilder() {
	const candidates = [];
	if (process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE) {
		candidates.push(process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE);
	}

	const errors = [];
	for (const candidate of unique(candidates)) {
		try {
			return { ...(await importModule(candidate)), source: candidate };
		} catch (error) {
			errors.push(`${candidate}: ${error.message}`);
		}
	}

	if (errors.length > 0) {
		process.stderr.write(`HOMEBOY_WP_CODEBOX_CORE_MODULE could not be loaded; using bundled WP Codebox bench recipe builder.\n${errors.map((error) => `- ${error}`).join('\n')}\n`);
	}

	return {
		buildWordPressBenchRecipe: buildBundledWordPressBenchRecipe,
		source: 'bundled',
	};
}

async function importModule(specifier) {
	if (specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('file:')) {
		return import(specifier.startsWith('file:') ? specifier : pathToFileURL(resolve(specifier)).href);
	}
	return import(specifier);
}

function unique(values) {
	return [...new Set(values.filter(Boolean))];
}
