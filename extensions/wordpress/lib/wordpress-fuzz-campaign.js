'use strict';

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
	DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS,
	DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS,
	WP_CODEBOX_FUZZ_SUITE_SCHEMA,
	WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzSuiteTaskRequest,
} = require('./wp-codebox-fuzz-run');

const WORDPRESS_FUZZ_CAMPAIGN_SCHEMA = 'homeboy/wordpress-fuzz-campaign/v1';
const HOMEBOY_FUZZ_WORKLOAD_SCHEMA = 'homeboy/fuzz-workload/v1';
const WORDPRESS_FUZZ_PLAN_RESULT_GAP_REPORT_SCHEMA = 'homeboy/wordpress-fuzz-plan-result-gap-report/v1';

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
	const plan = buildWordPressFuzzPlanFromSurfaces(discovery, planOptions);
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
			production_campaign: production || undefined,
		},
		artifacts: input.artifacts || options.artifacts,
		production,
	});
	const suiteInput = wpCodeboxFuzzSuiteInput({
		...(objectOrUndefined(options.suiteInput) || objectOrUndefined(options.suite_input) || {}),
		...(objectOrUndefined(input.suite_input || input.suiteInput) || {}),
		id: input.suite_id || input.suiteId || options.suiteId || options.suite_id || `${plan.id}-suite`,
		target: input.target || options.target,
		coverage: campaignCoverageRequest(coverageManifest),
		homeboyFuzzWorkload: workload,
		metadata: {
			...(objectOrUndefined(options.suiteInput?.metadata) || objectOrUndefined(options.suite_input?.metadata) || {}),
			...(objectOrUndefined(input.suite_input?.metadata) || objectOrUndefined(input.suiteInput?.metadata) || {}),
			coverage_manifest: coverageManifest,
			aggregation_hooks: campaignAggregationHooks(),
			production_campaign: production || undefined,
			output_requirements: production ? productionOutputRequirements() : undefined,
		},
	});
	const taskRequest = wpCodeboxFuzzSuiteTaskRequest({
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
			task_request: taskRequest,
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
		metadata: {
			...(objectOrUndefined(input.metadata) || {}),
			coverage_manifest: input.coverageManifest || input.coverage_manifest,
			aggregation_hooks: campaignAggregationHooks(),
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
	return {
		expected: [
			{ name: 'wordpress_fuzz_result', role: 'normalized_fuzz_result', semantic_key: 'fuzz.result.normalized', required: true },
			{ name: 'wordpress_fuzz_coverage', role: 'coverage', semantic_key: 'fuzz.coverage', required: true },
			{ name: 'wordpress-hotspots', role: 'hotspot_summary', semantic_key: 'fuzz.hotspot.summary', schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA, required: hotspotRequired },
			{ name: 'wordpress_performance_observation', role: 'fuzz_report', semantic_key: 'fuzz.performance', required: false },
		],
	};
}

function productionOutputRequirements() {
	return {
		required_artifact_keys: ['fuzz.coverage', 'fuzz.hotspot.summary'],
		required_artifact_schemas: [WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA],
	};
}

function productionExpectedArtifacts(production = false) {
	if (!production) {
		return undefined;
	}
	return [...new Set([...DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS, 'wordpress-hotspots'])];
}

function productionArtifactDeclarations(production = false) {
	if (!production) {
		return undefined;
	}
	return DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS.map((artifact) => artifact.semantic_key === 'fuzz.hotspot.summary'
		? { ...artifact, required: true }
		: artifact);
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

function arrayOf(value) {
	return Array.isArray(value) ? value.filter(Boolean) : [];
}

function objectOrUndefined(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

module.exports = {
	HOMEBOY_FUZZ_WORKLOAD_SCHEMA,
	WORDPRESS_FUZZ_CAMPAIGN_SCHEMA,
	WORDPRESS_FUZZ_PLAN_RESULT_GAP_REPORT_SCHEMA,
	aggregateWordPressFuzzCampaignResult,
	buildWordPressFuzzCampaignWorkload,
	compileWordPressFuzzCampaign,
	detectWordPressFuzzPlanResultGaps,
};
