'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');

/**
 * Internal dependencies
 */
const {
	WORDPRESS_FUZZ_PLAN_SCHEMA,
	normalizeWordPressFuzzPlan,
} = require('./wordpress-fuzz-schemas');
const { buildWpCodeboxFuzzPlanRecipe } = require('./wp-codebox-fuzz-plan');
const {
	normalizeWpCodeboxFuzzRunResult,
	wpCodeboxFuzzRunInput,
	wpCodeboxFuzzRunTaskRequest,
} = require('./wp-codebox-fuzz-run');
const { aggregateWordPressFuzzCoverage } = require('./wordpress-fuzz-coverage-aggregate');

const WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA = 'homeboy/wordpress-fuzz-runner-result/v1';

function readWordPressFuzzRunnerEnv(env = process.env) {
	return stripUndefined({
		workloadPath: env.HOMEBOY_FUZZ_WORKLOAD_PATH,
		workloadId: env.HOMEBOY_FUZZ_WORKLOAD_ID,
		runId: env.HOMEBOY_FUZZ_RUN_ID,
		seed: env.HOMEBOY_FUZZ_SEED,
		maxDuration: env.HOMEBOY_FUZZ_MAX_DURATION,
	});
}

function buildWordPressFuzzRunnerResult(options = {}) {
	const env = options.env || readWordPressFuzzRunnerEnv();
	const workload = options.workload || readJsonFile(requiredString(env.workloadPath, 'HOMEBOY_FUZZ_WORKLOAD_PATH'));
	const runId = requiredString(env.runId || workload.run_id || workload.runId || workload.id, 'HOMEBOY_FUZZ_RUN_ID');
	const workloadId = env.workloadId || workload.workload_id || workload.workloadId || workload.id || null;
	const seed = env.seed || workload.seed || null;
	const maxDuration = numericValue(env.maxDuration ?? workload.max_duration ?? workload.maxDuration);
	const plan = normalizeRunnerPlan(workload.plan || workload.fuzz_plan || workload.fuzzPlan || workload);
	const wpCodeboxInput = buildWpCodeboxInput({ workload, plan, runId, workloadId, seed, maxDuration });
	const taskRequest = wpCodeboxFuzzRunTaskRequest({
		taskId: runId,
		input: wpCodeboxInput,
		provider: workload.provider,
		runtimeId: workload.runtime_id || workload.runtimeId || 'wp-codebox',
	});
	const codeboxPlanRecipe = buildCodeboxPlanRecipe(workload);
	const codeboxResult = normalizeCodeboxResult(workload);
	const coverage = aggregateCoverage(workload, codeboxResult);
	const status = codeboxResult?.succeeded === false || hasCoverageFailures(coverage) ? 'failed' : (codeboxResult?.status || 'planned');

	return stripUndefined({
		schema: WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
		status,
		succeeded: status === 'planned' ? undefined : !['failed', 'errored'].includes(String(status).toLowerCase()),
		run_id: runId,
		workload_id: workloadId,
		seed,
		max_duration_seconds: maxDuration,
		plan_id: plan.id,
		wp_codebox_input: wpCodeboxInput,
		wp_codebox_task_request: taskRequest,
		wp_codebox_plan_recipe: codeboxPlanRecipe,
		wp_codebox_result: codeboxResult,
		coverage,
		metadata: objectOrUndefined(workload.metadata),
	});
}

function normalizeRunnerPlan(input) {
	if (input?.schema === WORDPRESS_FUZZ_PLAN_SCHEMA || Array.isArray(input?.targets)) {
		return normalizeWordPressFuzzPlan(input);
	}
	return normalizeWordPressFuzzPlan({
		schema: WORDPRESS_FUZZ_PLAN_SCHEMA,
		id: input?.id || input?.plan_id || input?.planId || 'wordpress-fuzz-plan',
		targets: normalizeArray(input?.targets),
		budget: objectOrUndefined(input?.budget),
		metadata: objectOrUndefined(input?.metadata),
	});
}

function buildWpCodeboxInput({ workload, plan, runId, workloadId, seed, maxDuration }) {
	return wpCodeboxFuzzRunInput({
		id: runId,
		target: workload.target || { type: 'wordpress', workload_id: workloadId },
		workload: stripUndefined({
			id: workloadId,
			plan_id: plan.id,
			discovery_id: plan.discovery_id,
			metadata: objectOrUndefined(workload.metadata),
		}),
		cases: flattenPlanCases(plan),
		seeds: seed ? [{ id: seed, value: seed }] : normalizeArray(workload.seeds),
		limits: stripUndefined({
			...(workload.limits || {}),
			max_duration_seconds: maxDuration,
		}),
		coverage: workload.coverage || { wordpress_fuzz_coverage: true },
		runtimeProfile: workload.runtime_profile || workload.runtimeProfile,
		artifacts: workload.artifacts,
		metadata: stripUndefined({ ...(workload.metadata || {}), runner: WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA }),
	});
}

function flattenPlanCases(plan) {
	return plan.targets.flatMap((target) => target.cases.map((testCase) => stripUndefined({
		...testCase,
		target_id: target.id,
		surface_id: target.surface_id,
		target_metadata: objectOrUndefined(target.metadata),
	})));
}

function buildCodeboxPlanRecipe(workload) {
	const plan = workload.wp_codebox_plan || workload.wpCodeboxPlan || workload.codebox_plan || workload.codeboxPlan;
	if (!plan) {
		return undefined;
	}
	return buildWpCodeboxFuzzPlanRecipe(plan);
}

function normalizeCodeboxResult(workload) {
	const result = workload.wp_codebox_result || workload.wpCodeboxResult || workload.result;
	return result ? normalizeWpCodeboxFuzzRunResult(result) : undefined;
}

function aggregateCoverage(workload, codeboxResult) {
	const coverageInput = workload.coverage_artifacts || workload.coverageArtifacts || codeboxResult?.coverage;
	if (!coverageInput) {
		return undefined;
	}
	return aggregateWordPressFuzzCoverage(coverageInput);
}

function hasCoverageFailures(coverage) {
	return Number(coverage?.totals?.failed || 0) > 0;
}

function readJsonFile(filePath) {
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function requiredString(value, name) {
	if (typeof value !== 'string' || value.trim() === '') {
		throw new Error(`${name} is required.`);
	}
	return value;
}

function normalizeArray(value) {
	return Array.isArray(value) ? value.filter(Boolean) : [];
}

function objectOrUndefined(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

function numericValue(value) {
	if (value === undefined || value === null || value === '') {
		return undefined;
	}
	const parsed = Number(value);
	return Number.isFinite(parsed) ? parsed : undefined;
}

function stripUndefined(value) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return value;
	}
	return Object.fromEntries(
		Object.entries(value).filter(([, entry]) => entry !== undefined)
	);
}

module.exports = {
	WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
	buildWordPressFuzzRunnerResult,
	readWordPressFuzzRunnerEnv,
};
