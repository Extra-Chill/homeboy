#!/usr/bin/env node
/**
 * External dependencies
 */
import { readFileSync } from 'node:fs';

/**
 * Internal dependencies
 */
import { buildWordPressPhpunitRecipe } from '../bench/wp-codebox-bench-recipe-builder.mjs';

const input = JSON.parse(readFileSync(0, 'utf8'));
const recipe = buildWordPressPhpunitRecipe(input.options ?? input);

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);
