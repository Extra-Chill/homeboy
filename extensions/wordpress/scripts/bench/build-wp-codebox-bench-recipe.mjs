#!/usr/bin/env node
/**
 * External dependencies
 */
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

/**
 * Internal dependencies
 */
import { loadCodeboxRecipeBuilder } from './wp-codebox-recipe-builder-loader.mjs';

const require = createRequire(import.meta.url);
const { normalizeFixtureProfileSiteSeeds } = require('../../lib/fixture-setup.js');

const input = JSON.parse(readFileSync(0, 'utf8'));
const { builder: buildWordPressBenchRecipe } = await loadCodeboxRecipeBuilder('buildWordPressBenchRecipe');
const options = { ...(input.options || {}) };
if (!options.extra_plugins && options.extraPlugins) {
	options.extra_plugins = options.extraPlugins;
}

const selectedScenarioIds = (process.env.HOMEBOY_BENCH_SCENARIOS || '')
	.split(',')
	.map((id) => id.trim())
	.filter(Boolean);
if (selectedScenarioIds.length) {
	options.scenarioIds = selectedScenarioIds;
}
if (selectedScenarioIds.length && Array.isArray(options.workloads)) {
	const selected = new Set(selectedScenarioIds);
	options.workloads = options.workloads.filter((workload) => selected.has(workload?.id));
}

const recipe = buildWordPressBenchRecipe(options);
const fixtureProfile = options.fixtureProfile ?? options.fixture_profile ?? options.wp_codebox_fixture_profile;
const siteSeeds = normalizeFixtureProfileSiteSeeds(fixtureProfile);
if (siteSeeds.length > 0) {
	recipe.inputs = recipe.inputs ?? {};
	recipe.inputs.siteSeeds = [
		...(Array.isArray(recipe.inputs.siteSeeds) ? recipe.inputs.siteSeeds : []),
		...siteSeeds,
	];
}
if (options.pluginRuntime && typeof options.pluginRuntime === 'object' && !Array.isArray(options.pluginRuntime)) {
	recipe.inputs = recipe.inputs ?? {};
	recipe.inputs.pluginRuntime = options.pluginRuntime;
}

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);
