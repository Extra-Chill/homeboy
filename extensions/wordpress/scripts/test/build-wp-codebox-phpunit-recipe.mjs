#!/usr/bin/env node
/**
 * External dependencies
 */
import { readFileSync } from 'node:fs';

/**
 * Internal dependencies
 */
import { loadCodeboxRecipeBuilder } from '../bench/wp-codebox-recipe-builder-loader.mjs';
import { applyWpCodeboxStepDiagnostics } from '../lib/wp-codebox-diagnostics-plan.mjs';

const input = JSON.parse(readFileSync(0, 'utf8'));
const { builder: buildWordPressPhpunitRecipe } = await loadCodeboxRecipeBuilder('buildWordPressPhpunitRecipe');
const options = input.options ?? input;
const recipe = buildWordPressPhpunitRecipe(options);
applyWpCodeboxStepDiagnostics(recipe, options);

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);
