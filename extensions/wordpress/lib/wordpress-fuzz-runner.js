'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
	WORDPRESS_FUZZ_PLAN_SCHEMA,
	normalizeWordPressFuzzPlan,
} = require('./wordpress-fuzz-schemas');
const { buildWpCodeboxFuzzPlanRecipe } = require('./wp-codebox-fuzz-plan');
const {
	normalizeWpCodeboxFuzzSuiteResult,
	runWpCodeboxFuzzSuite,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzSuiteTaskRequest,
} = require('./wp-codebox-fuzz-run');
const { aggregateWordPressFuzzCoverage } = require('./wordpress-fuzz-coverage-aggregate');

const WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA = 'homeboy/wordpress-fuzz-runner-result/v1';
const HOMEBOY_FUZZ_CAMPAIGN_SCHEMA = 'homeboy/fuzz-campaign/v1';

function readWordPressFuzzRunnerEnv(env = process.env) {
	return stripUndefined({
		workloadPath: env.HOMEBOY_FUZZ_WORKLOAD_PATH,
		workloadId: env.HOMEBOY_FUZZ_WORKLOAD_ID,
		runId: env.HOMEBOY_FUZZ_RUN_ID,
		seed: env.HOMEBOY_FUZZ_SEED,
		maxDuration: env.HOMEBOY_FUZZ_MAX_DURATION,
		resultsFile: env.HOMEBOY_FUZZ_RESULTS_FILE,
		wpCodeboxFuzzWorkloadRoot: env.WP_CODEBOX_FUZZ_WORKLOAD_ROOT,
	});
}

function buildWordPressFuzzRunnerResult(options = {}) {
	const context = buildWordPressFuzzRunnerContext(options);
	const codeboxResult = normalizeCodeboxResult(context.workload, { runId: context.runId });
	return buildWordPressFuzzRunnerSummary({ ...context, codeboxResult });
}

async function runWordPressFuzzRunnerResult(options = {}) {
	const context = buildWordPressFuzzRunnerContext(options);
	const codeboxResult = await resolveCodeboxResult(context, options);
	return buildWordPressFuzzRunnerSummary({ ...context, codeboxResult });
}

function buildWordPressFuzzRunnerContext(options = {}) {
	const env = options.env || readWordPressFuzzRunnerEnv();
	const workload = options.workload || readJsonFile(requiredString(env.workloadPath, 'HOMEBOY_FUZZ_WORKLOAD_PATH'));
	const runId = requiredString(env.runId || workload.run_id || workload.runId || workload.id, 'HOMEBOY_FUZZ_RUN_ID');
	const workloadId = env.workloadId || workload.workload_id || workload.workloadId || workload.id || null;
	const seed = env.seed || workload.seed || null;
	const maxDuration = numericValue(env.maxDuration ?? workload.max_duration ?? workload.maxDuration);
	const plan = normalizeRunnerPlan(workload.plan || workload.fuzz_plan || workload.fuzzPlan || workload);
	const instructions = fuzzSuiteInstructions({ workload, workloadId, runId });
	const wpCodeboxInput = buildWpCodeboxInput({ workload, plan, runId, workloadId, seed, maxDuration, instructions });
	const runtimeRequirements = wpCodeboxRuntimeRequirementsFromWorkload(workload, { env });
	const taskRequest = wpCodeboxFuzzSuiteTaskRequest({
		taskId: runId,
		input: wpCodeboxInput,
		provider: workload.provider,
		runtimeId: workload.runtime_id || workload.runtimeId || 'wp-codebox',
		runtimeRequirements,
		instructions,
	});
	const codeboxPlanRecipe = buildCodeboxPlanRecipe(workload);

	return {
		env,
		workload,
		runId,
		workloadId,
		seed,
		maxDuration,
		plan,
		wpCodeboxInput,
		runtimeRequirements,
		taskRequest,
		codeboxPlanRecipe,
	};
}

function buildWordPressFuzzRunnerSummary({
	workload,
	runId,
	workloadId,
	seed,
	maxDuration,
	plan,
	wpCodeboxInput,
	runtimeRequirements,
	taskRequest,
	codeboxPlanRecipe,
	codeboxResult,
}) {
	const coverage = aggregateCoverage(workload, codeboxResult);
	const status = normalizeRunnerStatus(codeboxResult, coverage);
	const homeboyFuzzCampaign = buildHomeboyFuzzCampaign({ runId, workloadId, plan, codeboxResult, status });

	return stripUndefined({
		schema: WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
		status,
		succeeded: !['failed', 'errored'].includes(String(status).toLowerCase()),
		run_id: runId,
		workload_id: workloadId,
		seed,
		max_duration_seconds: maxDuration,
		plan_id: plan.id,
		wp_codebox_input: wpCodeboxInput,
		wp_codebox_runtime_requirements: runtimeRequirements,
		wp_codebox_task_request: taskRequest,
		wp_codebox_plan_recipe: codeboxPlanRecipe,
		wp_codebox_result: codeboxResult,
		coverage,
		homeboy_fuzz_campaign: homeboyFuzzCampaign,
		metadata: objectOrUndefined(workload.metadata),
	});
}

async function resolveCodeboxResult(context, options = {}) {
	if (hasPrecomputedCodeboxResult(context.workload)) {
		return normalizeCodeboxResult(context.workload, { runId: context.runId });
	}

	const runner = options.runFuzzSuite || options.runFuzzRun || options.runRuntimeTask || options.runTask;
	if (typeof runner !== 'function') {
		return normalizeCodeboxResult(context.workload, { runId: context.runId });
	}

	return runWpCodeboxFuzzSuite({
		...options,
		taskId: context.runId,
		input: context.wpCodeboxInput,
		provider: context.workload.provider,
		runtimeId: context.workload.runtime_id || context.workload.runtimeId || 'wp-codebox',
		runtimeRequirements: context.runtimeRequirements,
		instructions: context.taskRequest.instructions,
		runFuzzSuite: runner,
	});
}

function fuzzSuiteInstructions({ workload, workloadId, runId }) {
	const label = workload.label || workloadId || workload.id || runId;
	return `Run WordPress fuzz suite ${label} and return the declared fuzz artifacts.`;
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

function buildWpCodeboxInput({ workload, plan, runId, workloadId, seed, maxDuration, instructions }) {
	const homeboyFuzzWorkload = workload.schema === 'homeboy/fuzz-workload/v1' ? workload : undefined;
	return wpCodeboxFuzzSuiteInput({
		id: runId,
		goal: instructions,
		target: workload.target || { type: 'wordpress', workload_id: workloadId },
		homeboyFuzzWorkload,
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
		metadata: stripUndefined({ ...(workload.metadata || {}), runner: WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA, workload: stripUndefined({ id: workloadId }) }),
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

function wpCodeboxRuntimeRequirementsFromWorkload(workload = {}, options = {}) {
	const context = objectOrUndefined(workload.metadata?.homeboy_runtime_context || workload.metadata?.homeboyRuntimeContext);
	const components = objectOrUndefined(context?.components);
	const workloadRoot = nonEmptyString(options.env?.wpCodeboxFuzzWorkloadRoot || options.env?.WP_CODEBOX_FUZZ_WORKLOAD_ROOT);
	const componentId = workload.target?.component
		|| workload.metadata?.fixture?.component
		|| workload.metadata?.fixture?.plugin
		|| workload.target?.slug;
	const component = componentId && components ? objectOrUndefined(components[componentId]) : undefined;
	const source = component?.path || component?.source;
	if ((!componentId || typeof source !== 'string' || source.trim() === '') && !workloadRoot) {
		return undefined;
	}
	const activation = workload.metadata?.fixture?.activation || firstCasePluginActivation(workload);
	return {
		extra_plugins: componentId && typeof source === 'string' && source.trim() !== '' ? [stripUndefined({
			slug: workload.target?.slug || componentId,
			source,
			path: source,
			pluginFile: activation,
			loadAs: 'plugin',
			activate: Boolean(activation),
			metadata: stripUndefined({
				component: componentId,
				rig_id: context.rig_id,
			}),
		})] : undefined,
		component_contracts: componentId && typeof source === 'string' && source.trim() !== '' ? [stripUndefined({
			slug: workload.target?.slug || componentId,
			path: source,
			pluginFile: activation,
			loadAs: 'plugin',
		})] : undefined,
		runtime_mounts: workloadRoot ? [{ source: workloadRoot, target: workloadRoot, mode: 'readonly' }] : undefined,
		runtime_env: workloadRoot ? { WP_CODEBOX_FUZZ_WORKLOAD_ROOT: workloadRoot } : undefined,
		metadata: stripUndefined({
			homeboy_runtime_context_schema: context?.schema,
			rig_id: context?.rig_id,
		}),
	};
}

function nonEmptyString(value) {
	return typeof value === 'string' && value.trim() !== '' ? value.trim() : undefined;
}

function firstCasePluginActivation(workload = {}) {
	for (const entry of normalizeArray(workload.cases)) {
		const activation = entry?.intent?.plugin?.activation;
		if (typeof activation === 'string' && activation.trim() !== '') {
			return activation;
		}
	}
	return undefined;
}

function normalizeCodeboxResult(workload, context = {}) {
	const result = precomputedCodeboxResult(workload);
	if (result) {
		return normalizeWpCodeboxFuzzSuiteResult(result);
	}
	return normalizeWpCodeboxFuzzSuiteResult({
		schema: 'wp-codebox/fuzz-suite-result/v1',
		request_id: context.runId,
		status: 'skipped',
		diagnostics: [
			{
				severity: 'warning',
				code: 'wp_codebox_fuzz_suite_execution_unsupported',
				message: 'WP Codebox exposes the public fuzz suite contract, but no merged execution API was available to this runner. Provide wp_codebox_suite_result in the workload or install a Codebox runtime that executes wp-codebox/run-fuzz-suite.',
			},
		],
	});
}

function hasPrecomputedCodeboxResult(workload = {}) {
	return Boolean(precomputedCodeboxResult(workload));
}

function precomputedCodeboxResult(workload = {}) {
	return workload.wp_codebox_result || workload.wpCodeboxResult || workload.wp_codebox_suite_result || workload.wpCodeboxSuiteResult || workload.result;
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

function normalizeRunnerStatus(codeboxResult, coverage) {
	if (codeboxResult.succeeded === false || hasCoverageFailures(coverage)) {
		return 'failed';
	}
	return codeboxResult.status || 'succeeded';
}

function buildHomeboyFuzzCampaign({ runId, workloadId, plan, codeboxResult, status }) {
	const diagnostics = normalizeArray(codeboxResult?.failures || codeboxResult?.metadata?.diagnostics || codeboxResult?.diagnostics);
	return stripUndefined({
		schema: HOMEBOY_FUZZ_CAMPAIGN_SCHEMA,
		id: runId,
		title: `WordPress fuzz campaign ${runId}`,
		safety_class: 'read_only',
		metadata: stripUndefined({
			workload_id: workloadId,
			plan_id: plan?.id,
			status,
			success: codeboxResult?.succeeded,
			wp_codebox_result_schema: codeboxResult?.result_schema,
			diagnostics: diagnostics.length > 0 ? diagnostics : undefined,
			artifact_refs: normalizeArray(codeboxResult?.artifacts),
			wordpress_fuzz_result: codeboxResult?.wordpress_fuzz_result,
		}),
	});
}

function writeHomeboyFuzzResultsFile(filePath, campaign) {
	if (!filePath) {
		return;
	}
	fs.mkdirSync(path.dirname(filePath), { recursive: true });
	fs.writeFileSync(filePath, `${JSON.stringify(campaign, null, 2)}\n`);
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
	HOMEBOY_FUZZ_CAMPAIGN_SCHEMA,
	WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
	buildWordPressFuzzRunnerResult,
	runWordPressFuzzRunnerResult,
	writeHomeboyFuzzResultsFile,
	readWordPressFuzzRunnerEnv,
};
