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
if (options.pluginRuntime && typeof options.pluginRuntime === 'object' && !Array.isArray(options.pluginRuntime)) {
	recipe.inputs = recipe.inputs ?? {};
	recipe.inputs.pluginRuntime = options.pluginRuntime;
}

if (shouldEmitCaseCheckpoints(options)) {
	emitCaseCheckpointSteps(recipe, options);
}

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);

function shouldEmitCaseCheckpoints(recipeOptions) {
	if (recipeOptions.checkpointCases === true || recipeOptions.caseCheckpoints === true) {
		return true;
	}

	const caseIsolation = recipeOptions.caseIsolation;
	return caseIsolation && typeof caseIsolation === 'object' && caseIsolation.checkpoints === true;
}

function emitCaseCheckpointSteps(targetRecipe, recipeOptions) {
	const workloads = Array.isArray(recipeOptions.workloads) ? recipeOptions.workloads : [];
	const caseIds = workloads
		.map((workload) => String(workload?.id || '').trim())
		.filter(Boolean);
	if (!caseIds.length || !Array.isArray(targetRecipe.workflow?.steps) || !targetRecipe.workflow.steps.length) {
		return;
	}

	const [benchStep, ...remainingSteps] = targetRecipe.workflow.steps;
	const checkpointName = String(recipeOptions.caseIsolation?.checkpointName || recipeOptions.checkpointName || 'wordpress-fuzz-case-baseline');
	const steps = [
		checkpointCreateStep(checkpointName),
		checkpointListStep(),
	];

	for (const caseId of caseIds) {
		steps.push(
			checkpointRestoreStep(checkpointName),
			caseBenchStep(benchStep, caseId),
			checkpointListStep(),
		);
	}

	targetRecipe.workflow.steps = [...steps, ...remainingSteps];
}

function checkpointCreateStep(name) {
	return {
		command: 'wp-codebox.checkpoint-create',
		args: [`name=${name}`],
	};
}

function checkpointRestoreStep(name) {
	return {
		command: 'wp-codebox.checkpoint-restore',
		args: [`name=${name}`],
	};
}

function checkpointListStep() {
	return {
		command: 'wp-codebox.checkpoint-list',
		args: [],
	};
}

function caseBenchStep(step, caseId) {
	return {
		...step,
		args: [
			...(Array.isArray(step.args) ? step.args : []),
			`scenario-ids-json=${JSON.stringify([caseId])}`,
		],
	};
}
