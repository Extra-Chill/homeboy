'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_FUZZ_POSTPROCESS_BINDING_SCHEMA,
	WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
} = require('../lib/wp-codebox-fuzz-run');

const {
	WORDPRESS_FUZZ_CAMPAIGN_SCHEMA,
	WORDPRESS_FUZZ_PLAN_RESULT_GAP_REPORT_SCHEMA,
	aggregateWordPressFuzzCampaignResult,
	compileWordPressFuzzCampaign,
	detectWordPressFuzzPlanResultGaps,
} = require('../lib/wordpress-fuzz-campaign');

function actionContract(action) {
	return {
		schema: 'wp-codebox/wordpress-runtime-action/v1',
		action,
		ability: `wp-codebox/runtime-action/${action}`,
	};
}

const codeboxRuntimeContracts = {
	schema: 'wp-codebox/wordpress-runtime-action-contracts/v1',
	actions: Object.fromEntries([
		'rest_request',
		'admin_page_load',
	].map((action) => [action, actionContract(action)])),
};

const campaign = compileWordPressFuzzCampaign({
	id: 'generic-wordpress-campaign',
	discovery_id: 'generic-runtime-surfaces',
	target: { type: 'wordpress-plugin', slug: 'sample-plugin' },
	metadata: { fixture: { activation: 'sample-plugin/sample-plugin.php' } },
	surfaces: [
		{ type: 'rest_route', route: '/wp/v2/posts', methods: ['GET', 'POST'] },
		{ type: 'admin_page', path: '/wp-admin/edit.php' },
	],
}, {
	taskId: 'generic-wordpress-campaign-task',
	plan: { seed: 'seed-1', codeboxRuntimeContracts },
});

assert.equal(campaign.schema, WORDPRESS_FUZZ_CAMPAIGN_SCHEMA);
assert.equal(campaign.discovery.schema, 'homeboy/wordpress-surface-discovery/v1');
assert.equal(campaign.plan.schema, 'wordpress-fuzz-plan/v1');
assert.equal(campaign.coverage_manifest.schema, 'homeboy/wordpress-fuzz-coverage-manifest/v1');
assert.equal(campaign.workload.schema, 'homeboy/fuzz-workload/v1');
assert.equal(campaign.wp_codebox.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(campaign.wp_codebox.input.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(campaign.wp_codebox.execution_request.schema, 'homeboy/wp-codebox-fuzz-execution/v1');
assert.equal(campaign.wp_codebox.execution_request.input.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(campaign.wp_codebox.execution_request.input.cases[0].target.kind, 'runtime-action');
assert.equal(campaign.wp_codebox.execution_request.input.cases[0].target.entrypoint, 'admin_page');
assert.equal(campaign.wp_codebox.execution_request.input.cases[0].input.type, 'admin_page');
assert.equal(campaign.wp_codebox.execution_request.input.cases[0].phases, undefined);
assert(campaign.wp_codebox.input.metadata.coverage_manifest.surfaces.length > 0);
assert.equal(campaign.workload.metadata.runtime_operations.schema, 'homeboy/wordpress-fuzz-runtime-workload-operation-summary/v1');
assert.equal(campaign.workload.metadata.runtime_operations.by_family.rest, 1);
assert.equal(campaign.workload.metadata.runtime_operations.by_family.admin_page, 1);
assert.equal(campaign.workload.metadata.runtime_operations.by_status.ready, 2);
assert.equal(campaign.wp_codebox.input.metadata.runtime_operations.total, 2);
assert.deepEqual(Object.keys(campaign.aggregation_hooks).sort(), ['coverage', 'gaps', 'performance']);
assert.equal(campaign.metadata.surface_count, 2);
assert.equal(campaign.metadata.target_count, 2);
assert.equal(campaign.metadata.case_count, 2);
assert(!JSON.stringify(campaign).includes('woocommerce'), 'campaign compiler must stay product-agnostic');

const plannedCaseIds = campaign.plan.targets.flatMap((target) => target.cases.map((testCase) => testCase.id));
const result = {
	wordpress_fuzz_result: {
		cases: [{
			id: plannedCaseIds[0],
			surface_id: campaign.plan.targets[0].surface_id,
			status: 'passed',
		}],
	},
};
const gaps = detectWordPressFuzzPlanResultGaps({
	plan: campaign.plan,
	result,
	coverage: {
		gapReport: { items: [{ id: 'route:/wp/v2/posts', status: 'skipped' }] },
	},
});

assert.equal(gaps.schema, WORDPRESS_FUZZ_PLAN_RESULT_GAP_REPORT_SCHEMA);
assert.equal(gaps.totals.planned_cases, plannedCaseIds.length);
assert.equal(gaps.totals.result_cases, 1);
assert.equal(gaps.totals.missing_cases, plannedCaseIds.length - 1);
assert.equal(gaps.totals.coverage_gaps, 1);
assert(gaps.missing_surfaces.length >= 1);

const aggregate = aggregateWordPressFuzzCampaignResult({
	campaign,
	result,
	coverage: { exercised: [campaign.coverage_manifest.surfaces[0].id] },
	performance: { status: 'passed', metrics: { query_count: 1 } },
});
assert.equal(aggregate.schema, 'homeboy/wordpress-fuzz-campaign-result-aggregate/v1');
assert.equal(aggregate.coverage.schema, 'homeboy/wordpress-fuzz-coverage-aggregate/v1');
assert.equal(aggregate.performance.schema, 'homeboy/wordpress-performance-observation/v1');
assert.equal(aggregate.gaps.schema, WORDPRESS_FUZZ_PLAN_RESULT_GAP_REPORT_SCHEMA);

const readOnlyCampaign = compileWordPressFuzzCampaign({
	id: 'read-only-campaign',
	mutation_mode: 'read_only',
	surfaces: [
		{ id: 'rest:campaign', type: 'rest_route', route: '/wp/v2/posts', method: 'GET' },
	],
}, {
	plan: { runtimeCapabilities: { capabilities: ['crud', 'snapshot', 'restore', 'reset'] } },
});
assert.equal(readOnlyCampaign.plan.metadata.mutation_mode, 'read_only');

const optInCampaign = compileWordPressFuzzCampaign({
	id: 'opt-in-campaign',
	mutation_mode: 'isolated',
	surfaces: [
		{ id: 'rest:opt-in-items', type: 'rest_route', route: '/example/v1/items/(?P<id>[\\d]+)', method: 'DELETE' },
	],
	fixture_plan: { id: 'campaign-fixtures', refs: [{ id: 'items', path: 'fixtures/items.json' }] },
	rest_mutation_opt_ins: {
		id: 'campaign-rest-opt-ins',
		entries: [
			{ id: 'post-item', route: '/example/v1/items/(?P<id>[\\d]+)', method: 'POST' },
			{ id: 'patch-item', route: '/example/v1/items/(?P<id>[\\d]+)', method: 'PATCH' },
			{ id: 'delete-item', route: '/example/v1/items/(?P<id>[\\d]+)', method: 'DELETE' },
		],
	},
}, {
	plan: {
		runtimeCapabilities: { capabilities: ['rest', 'checkpoint', 'rest-rollback', 'restore'] },
		runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready', operationKinds: ['mutation'], mutationIsolation: true, deleteBoundary: true },
		fixture_bindings: { 'rest:opt-in-items': { route_params: { id: 42 } } },
	},
});
assert.equal(optInCampaign.wp_codebox.input.metadata.fixture_plan.id, 'campaign-fixtures');
assert.equal(optInCampaign.wp_codebox.input.metadata.rest_mutation_opt_ins.id, 'campaign-rest-opt-ins');
assert.equal(optInCampaign.wp_codebox.execution_request.input.metadata.rest_mutation_opt_ins.entries.length, 3);

const productionCampaign = compileWordPressFuzzCampaign({
	id: 'production-campaign',
	production: true,
	surfaces: [
		{ id: 'rest:production', type: 'rest_route', route: '/wp/v2/posts', method: 'GET' },
	],
});
assert.equal(productionCampaign.workload.metadata.production_campaign, true);
assert.equal(productionCampaign.wp_codebox.input.metadata.production_campaign, true);
assert.deepEqual(productionCampaign.wp_codebox.input.metadata.output_requirements.required_artifact_keys, ['fuzz.coverage', 'fuzz.hotspot.summary']);
assert.deepEqual(productionCampaign.wp_codebox.input.metadata.output_requirements.required_postprocess_outputs, ['fuzz.coverage', 'fuzz.hotspot.summary', 'fuzz.coverage.gap_report', 'fuzz.hotspot.codebox']);
assert.deepEqual(productionCampaign.wp_codebox.input.metadata.output_requirements.required_artifact_schemas, [WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA]);
assert.equal(productionCampaign.wp_codebox.input.metadata.output_requirements.production_campaign, true);
assert.equal(productionCampaign.wp_codebox.input.metadata.postprocess_binding.schema, WORDPRESS_FUZZ_POSTPROCESS_BINDING_SCHEMA);
assert.equal(productionCampaign.workload.postprocess_binding.schema, WORDPRESS_FUZZ_POSTPROCESS_BINDING_SCHEMA);
assert.equal(productionCampaign.workload.metadata.postprocess_binding.schema, WORDPRESS_FUZZ_POSTPROCESS_BINDING_SCHEMA);
assert.deepEqual(
	productionCampaign.wp_codebox.input.metadata.postprocess_binding.outputs.map((artifact) => artifact.semantic_key).sort(),
	['fuzz.coverage', 'fuzz.coverage.gap_report', 'fuzz.hotspot.codebox', 'fuzz.hotspot.summary'].sort()
);
assert.equal(productionCampaign.workload.artifacts.expected.find((artifact) => artifact.semantic_key === 'fuzz.hotspot.summary').name, 'homeboy-hotspot-summary');
assert.equal(productionCampaign.workload.artifacts.expected.find((artifact) => artifact.semantic_key === 'fuzz.hotspot.summary').required, true);
assert.equal(productionCampaign.workload.artifacts.expected.find((artifact) => artifact.semantic_key === 'fuzz.hotspot.codebox').name, 'wordpress-hotspots');
assert.equal(productionCampaign.workload.artifacts.expected.find((artifact) => artifact.semantic_key === 'fuzz.coverage.gap_report').name, 'wordpress-fuzz-gap-report');
assert.equal(productionCampaign.wp_codebox.execution_request.expected_artifacts.includes('wordpress-hotspots'), true);
assert.equal(productionCampaign.wp_codebox.execution_request.expected_artifacts.includes('homeboy-hotspot-summary'), true);
assert.equal(productionCampaign.wp_codebox.execution_request.expected_artifacts.includes('wordpress-fuzz-gap-report'), true);
assert.equal(productionCampaign.wp_codebox.execution_request.artifact_declarations.find((artifact) => artifact.semantic_key === 'fuzz.hotspot.summary').required, true);
assert.equal(productionCampaign.wp_codebox.execution_request.artifact_declarations.find((artifact) => artifact.semantic_key === 'fuzz.hotspot.codebox').schema, WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA);
assert.equal(productionCampaign.wp_codebox.execution_request.artifact_declarations.find((artifact) => artifact.semantic_key === 'fuzz.coverage.gap_report').required, true);

console.log('wordpress fuzz campaign smoke passed');
