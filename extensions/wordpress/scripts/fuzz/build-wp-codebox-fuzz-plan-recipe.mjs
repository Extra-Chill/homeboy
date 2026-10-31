#!/usr/bin/env node
/**
 * External dependencies
 */
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';

/**
 * Internal dependencies
 */
const require = createRequire(import.meta.url);
const { buildWpCodeboxFuzzPlanRecipe } = require('../../lib/wp-codebox-fuzz-plan');

const input = JSON.parse(readFileSync(0, 'utf8'));
const recipe = buildWpCodeboxFuzzPlanRecipe(input);

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);
