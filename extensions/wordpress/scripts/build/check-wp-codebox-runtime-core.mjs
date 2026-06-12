#!/usr/bin/env node

import { loadCodeboxRecipeBuilder } from '../bench/wp-codebox-recipe-builder-loader.mjs';

try {
	const { source } = await loadCodeboxRecipeBuilder('buildWordPressBenchRecipe');
	console.log(`WP Codebox runtime core ready: ${source}`);
} catch (error) {
	console.error(error.message);
	process.exit(1);
}
