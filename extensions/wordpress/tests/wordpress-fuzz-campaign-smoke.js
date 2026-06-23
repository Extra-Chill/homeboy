'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_FUZZ_CAMPAIGN_SCHEMA,
	WORDPRESS_FUZZ_PLAN_RESULT_GAP_REPORT_SCHEMA,
	aggregateWordPressFuzzCampaignResult,
	compileWordPressFuzzCampaign,
	detectWordPressFuzzPlanResultGaps,
} = require('../lib/wordpress-fuzz-campaign');

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
	plan: { seed: 'seed-1' },
});

assert.equal(campaign.schema, WORDPRESS_FUZZ_CAMPAIGN_SCHEMA);
assert.equal(campaign.discovery.schema, 'homeboy/wordpress-surface-discovery/v1');
assert.equal(campaign.plan.schema, 'wordpress-fuzz-plan/v1');
assert.equal(campaign.coverage_manifest.schema, 'homeboy/wordpress-fuzz-coverage-manifest/v1');
assert.equal(campaign.workload.schema, 'homeboy/fuzz-workload/v1');
assert.equal(campaign.wp_codebox.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(campaign.wp_codebox.input.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(campaign.wp_codebox.task_request.executor.config.runtime_task.input.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(campaign.wp_codebox.task_request.executor.config.runtime_task.input.cases[0].target.entrypoint, 'wordpress.run-fuzz-case');
assert.equal(campaign.wp_codebox.task_request.executor.config.runtime_task.input.cases[0].phases.action[0].command, 'wordpress.run-fuzz-case');
assert(campaign.wp_codebox.input.metadata.coverage_manifest.surfaces.length > 0);
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

console.log('wordpress fuzz campaign smoke passed');
