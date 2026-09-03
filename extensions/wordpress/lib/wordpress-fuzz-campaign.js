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
	buildWordPressRuntimeSurfaceCoverageManifest,
	normalizeWordPressRuntimeSurfaceDiscovery,
} = require('./wordpress-runtime-surface-discovery');
const {
	buildWordPressFuzzPlanFromSurfaces,
} = require('./wordpress-fuzz-plan-from-surfaces');
const {
	aggregateWordPressFuzzCoverage,
} = require('./wordpress-fuzz-coverage-aggregate');
const {
	buildWordPressPerformanceObservation,
} = require('./wordpress-performance-observation-aggregate');
const {
	runWordPressLiveSurfaceDiscoveryWorkload,
} = require('./wordpress-live-surface-discovery');
const {
	WP_CODEBOX_FUZZ_SUITE_SCHEMA,
	WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
	runWpCodeboxFuzzSuite,
	wordpressFuzzPostprocessArtifactDeclarations,
	wordpressFuzzPostprocessBinding,
	wordpressFuzzPostprocessExpectedArtifacts,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzExecutionRequest,
} = require('./wp-codebox-fuzz-run');
const {
	summarizeWordPressFuzzRuntimeWorkloadOperations,
} = require('./wordpress-fuzz-runtime-workload-operations');

const WORDPRESS_FUZZ_CAMPAIGN_SCHEMA = 'homeboy/wordpress-fuzz-campaign/v1';
const WORDPRESS_FUZZ_CAMPAIGN_RUN_SCHEMA = 'homeboy/wordpress-fuzz-campaign-run/v1';
const WORDPRESS_FUZZ_CAMPAIGN_ARTIFACT_VALIDATION_SCHEMA = 'homeboy/wordpress-fuzz-campaign-artifact-validation/v1';
const HOMEBOY_FUZZ_WORKLOAD_SCHEMA = 'homeboy/fuzz-workload/v1';
const WORDPRESS_FUZZ_PLAN_RESULT_GAP_REPORT_SCHEMA = 'homeboy/wordpress-fuzz-plan-result-gap-report/v1';
const DESTRUCTIVE_CAMPAIGN_REQUIRED_ARTIFACTS = [
	{ semantic_key: 'fuzz.disposable.sandbox_isolation_proof', label: 'sandbox isolation proof' },
	{ semantic_key: 'fuzz.mutation.isolation', label: 'mutation isolation artifact' },
	{ semantic_key: 'fuzz.delete.boundary', label: 'delete boundary artifact' },
	{ semantic_key: 'fuzz.external_http.guardrail', label: 'external side-effect guardrail' },
	{ semantic_key: 'fuzz.runtime.access', label: 'runtime access artifact' },
	{ semantic_key: 'fuzz.coverage', alternatives: ['fuzz.coverage.summary'], label: 'coverage artifact' },
	{ semantic_key: 'fuzz.hotspot.summary', alternatives: ['fuzz.hotspot.codebox'], label: 'hotspots artifact' },
];

async function runWordPressFuzzCampaign(input = {}, options = {}) {
	const discovery = await resolveCampaignDiscovery(input, options);
	const destructive = isDestructiveCampaign(input, options);
	const campaign = compileWordPressFuzzCampaign({
		...input,
		discovery,
		production: input.production || input.production_campaign || destructive || undefined,
	}, options);
	const result = await runWpCodeboxFuzzSuite({
		...(objectOrUndefined(options.execution) || objectOrUndefined(options.execute) || {}),
		...(objectOrUndefined(input.execution) || objectOrUndefined(input.execute) || {}),
		taskId: campaign.wp_codebox.execution_request.task_id || campaign.id,
		input: campaign.wp_codebox.input,
		request: campaign.wp_codebox.execution_request,
		artifactDeclarations: campaign.wp_codebox.execution_request.artifact_declarations,
		expectedArtifacts: campaign.wp_codebox.execution_request.expected_artifacts,
		runtimeId: input.runtime_id || input.runtimeId || options.runtime_id || options.runtimeId || 'wp-codebox',
		runFuzzSuite: input.runFuzzSuite || options.runFuzzSuite || input.run_fuzz_suite || options.run_fuzz_suite,
	});
	const aggregate = aggregateWordPressFuzzCampaignResult({
		campaign,
		result,
		coverage: result.coverage || result.coverage_summary || result.artifacts,
		performance: result.performance || result.performance_observation || result.performanceObservation,
	});
	const artifactValidation = validateWordPressFuzzCampaignArtifacts({
		campaign,
		result,
		destructive,
		requiredArtifacts: options.requiredArtifacts || options.required_artifacts || input.requiredArtifacts || input.required_artifacts,
	});
	const status = resultStatus(result, artifactValidation);
	const summary = stripUndefined({
		schema: WORDPRESS_FUZZ_CAMPAIGN_RUN_SCHEMA,
		status,
		succeeded: status === 'succeeded',
		campaign,
		result,
		aggregate,
		artifact_validation: artifactValidation,
		metadata: {
			destructive_campaign: destructive || undefined,
		},
	});
	writeCampaignSummary(summary, input.summary_path || input.summaryPath || options.summary_path || options.summaryPath);
	return summary;
}

async function resolveCampaignDiscovery(input = {}, options = {}) {
	const supplied = input.discovery || input.surfaceDiscovery || input.surface_discovery || options.discovery || options.surfaceDiscovery || options.surface_discovery;
	if (supplied) {
		return supplied;
	}
	const liveDiscoveryOptions = objectOrUndefined(input.live_discovery || input.liveDiscovery || input.discovery_config || input.discoveryConfig || options.live_discovery || options.liveDiscovery || options.discovery_config || options.discoveryConfig);
	if (liveDiscoveryOptions) {
		return (await runWordPressLiveSurfaceDiscoveryWorkload(liveDiscoveryOptions)).artifact;
	}
	return input;
}

function validateWordPressFuzzCampaignArtifacts(input = {}) {
	const destructive = Boolean(input.destructive);
	const requiredArtifacts = normalizeRequiredCampaignArtifacts(
		input.requiredArtifacts || input.required_artifacts || (destructive ? DESTRUCTIVE_CAMPAIGN_REQUIRED_ARTIFACTS : [])
	);
	const artifacts = campaignResultArtifacts(input.result);
	const missing = requiredArtifacts.filter((requirement) => !hasSuccessfulCampaignArtifact(artifacts, requirement));
	return {
		schema: WORDPRESS_FUZZ_CAMPAIGN_ARTIFACT_VALIDATION_SCHEMA,
		status: missing.length === 0 ? 'passed' : 'failed',
		destructive_campaign: destructive,
		required_artifacts: requiredArtifacts,
		observed_artifacts: artifacts.map((artifact) => stripUndefined({
			name: artifact.name,
			role: artifact.role,
			semantic_key: artifact.semantic_key || artifact.semanticKey || artifact.metadata?.semantic_key || artifact.metadata?.semanticKey,
			schema: artifact.schema || artifact.metadata?.schema,
			status: artifact.status || artifact.metadata?.status,
		})),
		missing_artifacts: missing,
	};
}

function compileWordPressFuzzCampaign(input = {}, options = {}) {
	const production = Boolean(input.production || options.production || input.production_campaign || options.production_campaign);
	const discovery = normalizeWordPressRuntimeSurfaceDiscovery(
		input.discovery || input.surfaceDiscovery || input.surface_discovery || input,
		{
			...(objectOrUndefined(options.discovery) || {}),
			id: options.discoveryId || options.discovery_id || input.discovery_id || input.discoveryId,
			source: options.source || input.source,
		}
	);
	const planOptions = {
		...(objectOrUndefined(options.plan) || {}),
		...(objectOrUndefined(input.plan_options || input.planOptions) || {}),
	};
	if (input.mutation_mode || input.mutationMode || options.mutation_mode || options.mutationMode) {
		planOptions.mutation_mode = input.mutation_mode || input.mutationMode || options.mutation_mode || options.mutationMode;
	}
	const fixturePlan = input.fixture_plan || input.fixturePlan || options.fixture_plan || options.fixturePlan;
	const restMutationOptIns = input.rest_mutation_opt_ins || input.restMutationOptIns || input.rest_mutation_opt_in || input.restMutationOptIn || options.rest_mutation_opt_ins || options.restMutationOptIns || options.rest_mutation_opt_in || options.restMutationOptIn;
	if (restMutationOptIns) {
		planOptions.rest_mutation_opt_ins = restMutationOptIns;
	}
	const plan = buildWordPressFuzzPlanFromSurfaces(discovery, planOptions);
	const runtimeOperationSummary = summarizeWordPressFuzzRuntimeWorkloadOperations(plan);
	const coverageManifest = buildWordPressRuntimeSurfaceCoverageManifest(discovery);
	const workload = buildWordPressFuzzCampaignWorkload({
		id: input.workload_id || input.workloadId || `${plan.id}-workload`,
		label: input.label || options.label || 'WordPress fuzz campaign workload',
		target: input.target || options.target,
		plan,
		coverageManifest,
		metadata: {
			...(objectOrUndefined(input.metadata) || {}),
			...(objectOrUndefined(options.metadata) || {}),
			campaign_schema: WORDPRESS_FUZZ_CAMPAIGN_SCHEMA,
			runtime_operations: runtimeOperationSummary,
			production_campaign: production || undefined,
		},
		artifacts: input.artifacts || options.artifacts,
		postprocess_binding: input.postprocess_binding || input.postprocessBinding || options.postprocess_binding || options.postprocessBinding || (production ? wordpressFuzzPostprocessBinding() : undefined),
		production,
	});
	const suiteInput = wpCodeboxFuzzSuiteInput({
		...(objectOrUndefined(options.suiteInput) || objectOrUndefined(options.suite_input) || {}),
		...(objectOrUndefined(input.suite_input || input.suiteInput) || {}),
		id: input.suite_id || input.suiteId || options.suiteId || options.suite_id || `${plan.id}-suite`,
		target: input.target || options.target,
		coverage: campaignCoverageRequest(coverageManifest),
		homeboyFuzzWorkload: workload,
		fixture_plan: fixturePlan,
		rest_mutation_opt_ins: restMutationOptIns,
		metadata: {
			...(objectOrUndefined(options.suiteInput?.metadata) || objectOrUndefined(options.suite_input?.metadata) || {}),
			...(objectOrUndefined(input.suite_input?.metadata) || objectOrUndefined(input.suiteInput?.metadata) || {}),
			coverage_manifest: coverageManifest,
			fixture_plan: fixturePlan,
			rest_mutation_opt_ins: restMutationOptIns,
			runtime_operations: runtimeOperationSummary,
			aggregation_hooks: campaignAggregationHooks(),
			production_campaign: production || undefined,
			output_requirements: production ? productionOutputRequirements() : undefined,
			postprocess_binding: production ? wordpressFuzzPostprocessBinding() : undefined,
		},
	});
	const executionRequest = wpCodeboxFuzzExecutionRequest({
		...(objectOrUndefined(options.taskRequest) || objectOrUndefined(options.task_request) || {}),
		...(objectOrUndefined(input.task_request || input.taskRequest) || {}),
		taskId: input.task_id || input.taskId || options.taskId || options.task_id || `${plan.id}-wp-codebox-fuzz-suite`,
		input: suiteInput,
		artifactDeclarations: productionArtifactDeclarations(production),
		expectedArtifacts: productionExpectedArtifacts(production),
	});

	return {
		schema: WORDPRESS_FUZZ_CAMPAIGN_SCHEMA,
		id: input.id || options.id || `${plan.id}-campaign`,
		type: 'wordpress-fuzz-campaign',
		discovery,
		plan,
		coverage_manifest: coverageManifest,
		workload,
		wp_codebox: {
			schema: WP_CODEBOX_FUZZ_SUITE_SCHEMA,
			input: suiteInput,
			execution_request: executionRequest,
		},
		aggregation_hooks: campaignAggregationHooks(),
		metadata: {
			planner: 'homeboy/wordpress-fuzz-campaign-compiler/v1',
			surface_count: discovery.surfaces.length,
			target_count: plan.targets.length,
			case_count: plan.targets.reduce((total, target) => total + arrayOf(target.cases).length, 0),
		},
	};
}

function buildWordPressFuzzCampaignWorkload(input = {}) {
	const plan = input.plan || {};
	const runtimeOperationSummary = summarizeWordPressFuzzRuntimeWorkloadOperations(plan);
	return {
		schema: HOMEBOY_FUZZ_WORKLOAD_SCHEMA,
		id: input.id || `${plan.id || 'wordpress-fuzz-plan'}-workload`,
		label: input.label || 'WordPress fuzz campaign workload',
		target: input.target,
		plan: {
			...plan,
			targets: arrayOf(plan.targets).map((target) => ({
				...target,
				cases: arrayOf(target.cases).map((testCase) => wordpressFuzzPlanCaseToRuntimeCase(testCase, target)),
			})),
		},
		artifacts: input.artifacts || defaultCampaignArtifacts({ production: input.production }),
		postprocess_binding: objectOrUndefined(input.postprocess_binding || input.postprocessBinding),
		metadata: {
			...(objectOrUndefined(input.metadata) || {}),
			coverage_manifest: input.coverageManifest || input.coverage_manifest,
			runtime_operations: runtimeOperationSummary,
			aggregation_hooks: campaignAggregationHooks(),
			postprocess_binding: objectOrUndefined(input.postprocess_binding || input.postprocessBinding),
		},
	};
}

function wordpressFuzzPlanCaseToRuntimeCase(testCase = {}, target = {}) {
	return {
		...testCase,
		command: testCase.command || 'wordpress.run-fuzz-case',
		input: {
			...(objectOrUndefined(testCase.input) || {}),
			case_id: testCase.id || testCase.case_id,
			target_id: target.id,
			surface_id: testCase.surface_id || testCase.surfaceId || target.surface_id || target.surfaceId,
			intent: testCase.intent,
			operation_id: testCase.operation_id || testCase.operationId,
			operation: testCase.operation,
		},
		metadata: {
			...(objectOrUndefined(testCase.metadata) || {}),
			source_plan_case: true,
			target_type: target.type,
			executable: testCase.executable !== false && arrayOf(testCase.skip_reasons || testCase.skipReasons).length === 0,
		},
	};
}

function campaignCoverageRequest(coverageManifest = {}) {
	const surfaceIds = arrayOf(coverageManifest.surfaces).map((surface) => surface.id).filter(Boolean);
	return {
		expected: surfaceIds.length,
		surface_ids: surfaceIds,
	};
}

function campaignAggregationHooks() {
	return {
		coverage: {
			schema: 'homeboy/wordpress-fuzz-coverage-aggregate/v1',
			function: 'aggregateWordPressFuzzCoverage',
		},
		performance: {
			schema: 'homeboy/wordpress-performance-observation/v1',
			function: 'buildWordPressPerformanceObservation',
		},
		gaps: {
			schema: WORDPRESS_FUZZ_PLAN_RESULT_GAP_REPORT_SCHEMA,
			function: 'detectWordPressFuzzPlanResultGaps',
		},
	};
}

function defaultCampaignArtifacts(options = {}) {
	return defaultCampaignArtifactsWithOptions(options);
}

function defaultCampaignArtifactsWithOptions(options = {}) {
	const hotspotRequired = Boolean(options.production);
	const expected = [
		{ name: 'wordpress_fuzz_result', role: 'normalized_fuzz_result', semantic_key: 'fuzz.result.normalized', required: true },
		{ name: 'wordpress_fuzz_coverage', role: 'coverage', semantic_key: 'fuzz.coverage', required: true },
		{ name: 'wordpress-hotspots', role: 'hotspot_summary', semantic_key: 'fuzz.hotspot.summary', schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA, required: hotspotRequired },
		{ name: 'wordpress_performance_observation', role: 'fuzz_report', semantic_key: 'fuzz.performance', required: false },
	];
	if (options.production) {
		expected.push(...wordpressFuzzPostprocessBinding().outputs);
	}
	return {
		expected: dedupeArtifactsBySemanticKey(expected),
	};
}

function dedupeArtifactsBySemanticKey(artifacts = []) {
	const byKey = new Map();
	for (const artifact of artifacts) {
		byKey.set(artifact.semantic_key || artifact.name, artifact);
	}
	return [...byKey.values()];
}

function productionOutputRequirements() {
	return {
		required_artifact_keys: ['fuzz.coverage', 'fuzz.hotspot.summary'],
		required_postprocess_outputs: ['fuzz.coverage', 'fuzz.hotspot.summary', 'fuzz.coverage.gap_report', 'fuzz.hotspot.codebox'],
		required_artifact_schemas: [WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA],
		production_campaign: true,
	};
}

function productionExpectedArtifacts(production = false) {
	if (!production) {
		return undefined;
	}
	return wordpressFuzzPostprocessExpectedArtifacts();
}

function productionArtifactDeclarations(production = false) {
	if (!production) {
		return undefined;
	}
	return wordpressFuzzPostprocessArtifactDeclarations();
}

function aggregateWordPressFuzzCampaignResult(input = {}) {
	const coverage = aggregateWordPressFuzzCoverage({
		coverage_manifest: input.coverage_manifest || input.coverageManifest || input.campaign?.coverage_manifest,
		artifacts: input.coverage || input.artifacts || input.result || input.results,
	});
	const performance = buildWordPressPerformanceObservation(input.performance || input.performanceObservation || input.performance_observation || input);
	const gaps = detectWordPressFuzzPlanResultGaps({
		plan: input.plan || input.campaign?.plan,
		result: input.result || input.results,
		coverage,
	});
	return {
		schema: 'homeboy/wordpress-fuzz-campaign-result-aggregate/v1',
		coverage,
		performance,
		gaps,
	};
}

function detectWordPressFuzzPlanResultGaps(input = {}) {
	const plan = input.plan || {};
	const plannedCases = collectPlannedCases(plan);
	const resultCases = collectResultCases(input.result || input.results || input);
	const resultCaseIds = new Set(resultCases.map((testCase) => testCase.id || testCase.case_id || testCase.caseId).filter(Boolean));
	const resultSurfaceIds = new Set(resultCases.map((testCase) => testCase.surface_id || testCase.surfaceId).filter(Boolean));
	const missingCases = plannedCases.filter((testCase) => !resultCaseIds.has(testCase.id));
	const skippedCases = resultCases.filter((testCase) => normalizeStatus(testCase.status) === 'skipped');
	const failedCases = resultCases.filter((testCase) => normalizeStatus(testCase.status) === 'failed' || normalizeStatus(testCase.status) === 'error');
	const missingSurfaces = collectPlannedSurfaces(plan).filter((surface) => !resultSurfaceIds.has(surface.id));
	const coverageGaps = arrayOf(input.coverage?.coverage_gaps || input.coverage?.gapReport?.items || input.coverage?.gap_report?.items);

	return {
		schema: WORDPRESS_FUZZ_PLAN_RESULT_GAP_REPORT_SCHEMA,
		totals: {
			planned_cases: plannedCases.length,
			result_cases: resultCases.length,
			missing_cases: missingCases.length,
			skipped_cases: skippedCases.length,
			failed_cases: failedCases.length,
			planned_surfaces: collectPlannedSurfaces(plan).length,
			missing_surfaces: missingSurfaces.length,
			coverage_gaps: coverageGaps.length,
		},
		missing_cases: missingCases,
		skipped_cases: skippedCases,
		failed_cases: failedCases,
		missing_surfaces: missingSurfaces,
		coverage_gaps: coverageGaps,
	};
}

function collectPlannedCases(plan = {}) {
	return arrayOf(plan.targets).flatMap((target) => (
		arrayOf(target.cases).map((testCase) => ({
			id: testCase.id || testCase.case_id || testCase.caseId,
			target_id: target.id,
			surface_id: testCase.surface_id || testCase.surfaceId || target.surface_id || target.surfaceId,
			intent: testCase.intent,
			executable: testCase.executable !== false,
		}))
	));
}

function collectPlannedSurfaces(plan = {}) {
	const byId = new Map();
	for (const target of arrayOf(plan.targets)) {
		const id = target.surface_id || target.surfaceId || target.id;
		if (id) {
			byId.set(id, { id, target_id: target.id, type: target.type });
		}
	}
	return [...byId.values()];
}

function collectResultCases(result = {}) {
	if (Array.isArray(result)) {
		return result.flatMap(collectResultCases);
	}
	if (!objectOrUndefined(result)) {
		return [];
	}
	return [
		...arrayOf(result.cases),
		...arrayOf(result.wordpress_fuzz_result?.cases),
		...arrayOf(result.wordpressFuzzResult?.cases),
		...arrayOf(result.result?.cases),
		...arrayOf(result.result?.wordpress_fuzz_result?.cases),
		...arrayOf(result.result?.wordpressFuzzResult?.cases),
	];
}

function normalizeStatus(value) {
	return String(value || '').trim().toLowerCase().replace(/_/g, '-');
}

function normalizeRequiredCampaignArtifacts(requirements = []) {
	return arrayOf(requirements).map((requirement) => {
		const entry = typeof requirement === 'string' ? { semantic_key: requirement } : objectOrUndefined(requirement);
		return entry ? stripUndefined({
			semantic_key: entry.semantic_key || entry.semanticKey || entry.key,
			alternatives: arrayOf(entry.alternatives || entry.alternative_semantic_keys || entry.alternativeSemanticKeys),
			label: entry.label || entry.name,
		}) : undefined;
	}).filter((requirement) => requirement?.semantic_key);
}

function campaignResultArtifacts(result = {}) {
	return [
		...arrayOf(result.artifacts),
		...arrayOf(result.wp_codebox_result?.artifacts),
		...arrayOf(result.result?.artifacts),
		...arrayOf(result.derived_artifacts?.artifacts),
		...arrayOf(result.wp_codebox_result?.derived_artifacts?.artifacts),
	].filter(objectOrUndefined);
}

function hasSuccessfulCampaignArtifact(artifacts = [], requirement = {}) {
	const keys = new Set([requirement.semantic_key, ...arrayOf(requirement.alternatives)].filter(Boolean));
	return artifacts.some((artifact) => {
		const status = normalizeStatus(artifact.status || artifact.metadata?.status);
		const semanticKey = artifact.semantic_key || artifact.semanticKey || artifact.metadata?.semantic_key || artifact.metadata?.semanticKey;
		return keys.has(semanticKey) && !['failed', 'errored', 'error', 'missing'].includes(status);
	});
}

function isDestructiveCampaign(input = {}, options = {}) {
	const value = input.destructive || input.destructive_mode || input.destructiveMode || options.destructive || options.destructive_mode || options.destructiveMode;
	if (value !== undefined) {
		return Boolean(value);
	}
	const mutationMode = String(input.mutation_mode || input.mutationMode || options.mutation_mode || options.mutationMode || '').toLowerCase();
	return ['aggressive', 'destructive', 'production-destructive', 'production_destructive'].includes(mutationMode);
}

function resultStatus(result = {}, artifactValidation = {}) {
	if (artifactValidation.status === 'failed') {
		return 'failed';
	}
	const status = normalizeStatus(result.status);
	if (!status || ['succeeded', 'success', 'passed', 'ok'].includes(status)) {
		return 'succeeded';
	}
	return status;
}

function writeCampaignSummary(summary, summaryPath) {
	if (!summaryPath) {
		return;
	}
	const resolvedPath = path.resolve(String(summaryPath));
	fs.mkdirSync(path.dirname(resolvedPath), { recursive: true });
	fs.writeFileSync(resolvedPath, `${JSON.stringify(summary, null, 2)}\n`);
}

function stripUndefined(value) {
	if (!objectOrUndefined(value)) {
		return value;
	}
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function arrayOf(value) {
	return Array.isArray(value) ? value.filter(Boolean) : [];
}

function objectOrUndefined(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

module.exports = {
	HOMEBOY_FUZZ_WORKLOAD_SCHEMA,
	DESTRUCTIVE_CAMPAIGN_REQUIRED_ARTIFACTS,
	WORDPRESS_FUZZ_CAMPAIGN_ARTIFACT_VALIDATION_SCHEMA,
	WORDPRESS_FUZZ_CAMPAIGN_RUN_SCHEMA,
	WORDPRESS_FUZZ_CAMPAIGN_SCHEMA,
	WORDPRESS_FUZZ_PLAN_RESULT_GAP_REPORT_SCHEMA,
	aggregateWordPressFuzzCampaignResult,
	buildWordPressFuzzCampaignWorkload,
	compileWordPressFuzzCampaign,
	detectWordPressFuzzPlanResultGaps,
	runWordPressFuzzCampaign,
	validateWordPressFuzzCampaignArtifacts,
};
