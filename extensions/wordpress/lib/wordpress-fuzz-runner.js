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
const { normalizeWordPressFuzzRuntimeCapabilities } = require('./wordpress-fuzz-runtime-capabilities');
const {
	normalizeWpCodeboxFuzzSuiteResult,
	runWpCodeboxFuzzSuite,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzRuntimeTaskRequest,
	wpCodeboxFuzzSuiteTaskRequest,
} = require('./wp-codebox-fuzz-run');
const { aggregateWordPressFuzzCoverage } = require('./wordpress-fuzz-coverage-aggregate');

const WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA = 'homeboy/wordpress-fuzz-runner-result/v1';
const HOMEBOY_FUZZ_CAMPAIGN_SCHEMA = 'homeboy/fuzz-campaign/v1';
const HOMEBOY_FUZZ_CONTRACT_VERSION = 1;

function readWordPressFuzzRunnerEnv(env = process.env) {
	return stripUndefined({
		workloadPath: env.HOMEBOY_FUZZ_WORKLOAD_PATH,
		workloadId: env.HOMEBOY_FUZZ_WORKLOAD_ID,
		runId: env.HOMEBOY_FUZZ_RUN_ID,
		seed: env.HOMEBOY_FUZZ_SEED,
		maxDuration: env.HOMEBOY_FUZZ_MAX_DURATION,
		resultsFile: env.HOMEBOY_FUZZ_RESULTS_FILE,
		wpCodeboxFuzzWorkloadRoot: env.WP_CODEBOX_FUZZ_WORKLOAD_ROOT,
		wpCodeboxBin: env.HOMEBOY_WP_CODEBOX_BIN || env.WP_CODEBOX_BIN || env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN,
		wpCliBin: env.HOMEBOY_WP_CLI_BIN || env.WP_CLI_BIN,
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
	const runtimeCapabilities = normalizeWordPressFuzzRuntimeCapabilities(workload.runtime_capabilities || workload.runtimeCapabilities || workload.runtime_profile?.fuzz_runtime_capabilities || workload.runtimeProfile?.fuzzRuntimeCapabilities || []);
	const instructions = fuzzSuiteInstructions({ workload, workloadId, runId });
	const wpCodeboxInput = buildWpCodeboxInput({ workload, plan, runId, workloadId, seed, maxDuration, instructions, runtimeCapabilities });
	const runtimeRequirements = wpCodeboxRuntimeRequirementsFromWorkload(workload, { env });
	const runtimeTaskRequest = wpCodeboxFuzzRuntimeTaskRequest({
		taskId: runId,
		input: wpCodeboxInput,
		provider: workload.provider,
		runtimeId: workload.runtime_id || workload.runtimeId || 'wp-codebox',
		runtimeRequirements,
		instructions,
	});
	const taskRequest = runtimeTaskRequest.provider_request || wpCodeboxFuzzSuiteTaskRequest({
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
		runtimeCapabilities,
		wpCodeboxInput,
		runtimeRequirements,
		runtimeTaskRequest,
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
	runtimeCapabilities,
	wpCodeboxInput,
	runtimeRequirements,
	runtimeTaskRequest,
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
		wordpress_fuzz_runtime_capabilities: runtimeCapabilities,
		wp_codebox_runtime_requirements: runtimeRequirements,
		wp_codebox_task_request: taskRequest,
		wp_codebox_plan_recipe: codeboxPlanRecipe,
		fuzz_runtime_task_request: runtimeTaskRequest,
		fuzz_runtime_task_result: codeboxResult.runtime_task_result,
		wp_codebox_result: codeboxResult,
		coverage,
		observation_set: codeboxResult.observation_set,
		hotspot_summary: codeboxResult.hotspot_summary || coverage?.hotspot_summary,
		homeboy_fuzz_campaign: homeboyFuzzCampaign,
		metadata: objectOrUndefined(workload.metadata),
	});
}

async function resolveCodeboxResult(context, options = {}) {
	if (hasPrecomputedCodeboxResult(context.workload)) {
		return normalizeCodeboxResult(context.workload, { runId: context.runId });
	}

	const runner = options.runFuzzSuite || options.runRuntimeTask || options.runTask;
	return runWpCodeboxFuzzSuite({
		...options,
		env: context.env,
		taskId: context.runId,
		input: context.wpCodeboxInput,
		provider: context.workload.provider,
		runtimeId: context.workload.runtime_id || context.workload.runtimeId || 'wp-codebox',
		runtimeRequirements: context.runtimeRequirements,
		instructions: context.taskRequest.instructions,
		...(typeof runner === 'function' ? { runFuzzSuite: runner } : {}),
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

function buildWpCodeboxInput({ workload, plan, runId, workloadId, seed, maxDuration, instructions, runtimeCapabilities }) {
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
		metadata: stripUndefined({ ...(workload.metadata || {}), runner: WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA, runtime_capabilities: runtimeCapabilities, workload: stripUndefined({ id: workloadId }) }),
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
	return buildWpCodeboxFuzzRuntimeRequirements({
		workload,
		env: options.env,
	});
}

function buildWpCodeboxFuzzRuntimeRequirements({ workload = {}, env = {} } = {}) {
	const context = objectOrUndefined(workload.metadata?.homeboy_runtime_context || workload.metadata?.homeboyRuntimeContext);
	const components = objectOrUndefined(context?.components);
	const workloadRoot = nonEmptyString(env?.wpCodeboxFuzzWorkloadRoot || env?.WP_CODEBOX_FUZZ_WORKLOAD_ROOT);
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
	const pluginRequirement = buildWpCodeboxFuzzPluginRequirement({ workload, componentId, source, activation, context });
	return {
		extra_plugins: pluginRequirement ? [pluginRequirement.extraPlugin] : undefined,
		component_contracts: pluginRequirement ? [pluginRequirement.componentContract] : undefined,
		runtime_mounts: workloadRoot ? [{ source: workloadRoot, target: workloadRoot, mode: 'readonly' }] : undefined,
		runtime_env: workloadRoot ? { WP_CODEBOX_FUZZ_WORKLOAD_ROOT: workloadRoot } : undefined,
		metadata: stripUndefined({
			homeboy_runtime_context_schema: context?.schema,
			rig_id: context?.rig_id,
		}),
	};
}

function buildWpCodeboxFuzzPluginRequirement({ workload = {}, componentId, source, activation, context = {} } = {}) {
	if (!componentId || typeof source !== 'string' || source.trim() === '') {
		return undefined;
	}
	const slug = workload.target?.slug || componentId;
	const component = objectOrUndefined(context.components?.[componentId]);
	const wordpressExtension = objectOrUndefined(component?.extensions?.wordpress);
	const sourceSubpath = nonEmptyString(wordpressExtension?.wp_codebox_source_subpath || wordpressExtension?.wpCodeboxSourceSubpath);
	const sourceLayout = wpCodeboxSourceLayout({ source, sourceSubpath, wordpressExtension });
	return {
		extraPlugin: stripUndefined({
			slug,
			source,
			sourceRoot: sourceLayout.sourceRoot,
			sourceSubpath: sourceLayout.sourceSubpath,
			path: source,
			pluginFile: activation,
			loadAs: 'plugin',
			activate: Boolean(activation),
			metadata: stripUndefined({
				component: componentId,
				rig_id: context.rig_id,
			}),
		}),
		componentContract: stripUndefined({
			slug,
			path: source,
			sourceRoot: sourceLayout.sourceRoot,
			sourceSubpath: sourceLayout.sourceSubpath,
			pluginFile: activation,
			loadAs: 'plugin',
		}),
	};
}

function wpCodeboxSourceLayout({ source, sourceSubpath, wordpressExtension } = {}) {
	const normalizedSubpath = nonEmptyString(sourceSubpath);
	if (normalizedSubpath && source.endsWith(`/${normalizedSubpath}`)) {
		return {
			sourceRoot: source.slice(0, -normalizedSubpath.length - 1),
			sourceSubpath: normalizedSubpath,
		};
	}

	const configured = nonEmptyString(wordpressExtension?.wp_codebox_source_root || wordpressExtension?.wpCodeboxSourceRoot);
	if (configured && configured.startsWith('~/')) {
		return {};
	}

	if (configured && !configured.startsWith('~/')) {
		return {
			sourceRoot: configured,
			sourceSubpath: normalizedSubpath,
		};
	}

	return {};
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
	const derivedCoverage = codeboxResult?.derived_artifacts?.coverage_gap_reports;
	const coverageInput = workload.coverage_artifacts || workload.coverageArtifacts || codeboxResult?.coverage || derivedCoverage;
	if (!coverageInput) {
		return codeboxResult?.hotspot_summary ? aggregateWordPressFuzzCoverage({ hotspot_summary: codeboxResult.hotspot_summary }) : undefined;
	}
	const artifacts = coverageInput === derivedCoverage
		? normalizeArray(coverageInput)
		: [...normalizeArray(coverageInput), ...normalizeArray(derivedCoverage)];
	return aggregateWordPressFuzzCoverage({ artifacts, hotspot_summary: codeboxResult?.hotspot_summary });
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
		version: HOMEBOY_FUZZ_CONTRACT_VERSION,
		id: runId,
		title: `WordPress fuzz campaign ${runId}`,
		safety_class: deriveHomeboyFuzzSafetyClass(plan),
		metadata: stripUndefined({
			workload_id: workloadId,
			plan_id: plan?.id,
			status,
			success: codeboxResult?.succeeded,
			wp_codebox_result_schema: codeboxResult?.result_schema,
			diagnostics: diagnostics.length > 0 ? diagnostics : undefined,
			artifact_refs: normalizeArray(codeboxResult?.artifacts),
			observation_set: codeboxResult?.observation_set,
			hotspot_summary: codeboxResult?.hotspot_summary,
			wordpress_fuzz_result: codeboxResult?.wordpress_fuzz_result,
		}),
	});
}

function deriveHomeboyFuzzSafetyClass(plan = {}) {
	return strongestFuzzSafetyClass(fuzzSafetyClassCandidates(plan));
}

function fuzzSafetyClassCandidates(plan = {}) {
	const candidates = [fuzzSafetyCandidateFrom(plan), fuzzSafetyCandidateFrom(plan.metadata)];
	for (const target of normalizeArray(plan.targets)) {
		candidates.push(fuzzSafetyCandidateFrom(target), fuzzSafetyCandidateFrom(target.metadata));
		for (const testCase of normalizeArray(target.cases)) {
			candidates.push(fuzzSafetyCandidateFrom(testCase), fuzzSafetyCandidateFrom(testCase.metadata));
		}
	}
	return candidates.filter(Boolean);
}

function fuzzSafetyCandidateFrom(source = {}) {
	if (!source || typeof source !== 'object') {
		return undefined;
	}
	const safety = objectOrUndefined(source.safety) || {};
	const explicit = source.safety_class || source.safetyClass || safety.safety_class || safety.safetyClass || safety.class || safety.level || safety.mutation || source.mutation;
	const explicitClass = normalizeHomeboyFuzzSafetyClass(explicit);
	if (explicitClass) {
		return explicitClass;
	}
	if (source.destructive === true || safety.destructive === true || safety.level === 'destructive') {
		return 'destructive';
	}
	if (source.mutates === true || safety.mutates === true || normalizeArray(source.destructive_reasons || source.destructiveReasons || source.destructive_reason || source.destructiveReason).length > 0) {
		return 'isolated_mutation';
	}
	return undefined;
}

function strongestFuzzSafetyClass(candidates = []) {
	const rank = {
		read_only: 0,
		idempotent: 1,
		isolated_mutation: 2,
		destructive: 3,
	};
	return candidates.reduce((strongest, candidate) => (
		rank[candidate] > rank[strongest] ? candidate : strongest
	), 'read_only');
}

function normalizeHomeboyFuzzSafetyClass(value) {
	const label = String(value || '').trim().toLowerCase().replace(/[\s.-]+/g, '_');
	if (!label) {
		return undefined;
	}
	if (['read_only', 'readonly', 'read', 'safe', 'non_mutating', 'none'].includes(label)) {
		return 'read_only';
	}
	if (['idempotent', 'repeatable'].includes(label)) {
		return 'idempotent';
	}
	if (['isolated_mutation', 'isolated', 'mutation', 'mutating', 'write', 'requires_isolated_editor_draft', 'requires_explicit_opt_in'].includes(label)) {
		return 'isolated_mutation';
	}
	if (['destructive', 'delete', 'dangerous'].includes(label)) {
		return 'destructive';
	}
	return undefined;
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
