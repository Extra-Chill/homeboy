'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	WORDPRESS_FUZZ_CAMPAIGN_ARTIFACT_VALIDATION_SCHEMA,
	WORDPRESS_FUZZ_CAMPAIGN_RUN_SCHEMA,
	runWordPressFuzzCampaign,
	validateWordPressFuzzCampaignArtifacts,
} = require('../lib/wordpress-fuzz-campaign');

function artifact(semanticKey, name = semanticKey, schema) {
	return { name, semantic_key: semanticKey, schema, status: 'passed' };
}

(async () => {
	const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wordpress-fuzz-campaign-'));
	const summaryPath = path.join(tmpDir, 'campaign-summary.json');
	const seen = {};
	const summary = await runWordPressFuzzCampaign({
		id: 'production-destructive-campaign',
		destructive: true,
		summaryPath,
		target: { type: 'wordpress-plugin', slug: 'sample-plugin' },
		discovery: {
			id: 'live-wordpress-surfaces',
			surfaces: [
				{ id: 'rest:items-delete', type: 'rest_route', route: '/example/v1/items/(?P<id>[\\d]+)', method: 'DELETE' },
			],
		},
		rest_mutation_opt_ins: {
			entries: [{ id: 'delete-item', route: '/example/v1/items/(?P<id>[\\d]+)', method: 'DELETE' }],
		},
	}, {
		plan: {
			fixture_bindings: { 'rest:items-delete': { route_params: { id: 42 } } },
			runtimeCapabilities: { capabilities: ['rest', 'checkpoint', 'rest-rollback', 'restore'] },
			runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready', operationKinds: ['mutation'], mutationIsolation: true, deleteBoundary: true },
		},
		runFuzzSuite: async (request) => {
			seen.request = request;
			return {
				schema: 'wp-codebox/fuzz-suite-result/v1',
				request_id: request.task_id,
				status: 'succeeded',
				wordpress_fuzz_result: {
					cases: [{ id: request.input.cases[0].id, surface_id: 'rest:items-delete', status: 'passed' }],
				},
				hotspot_summary: {
					schema: 'wp-codebox/wordpress-hotspots/v1',
					hotspots: [{ id: 'rest:items-delete', metric: 'duration_ms', value: 1 }],
				},
				artifacts: [
					artifact('fuzz.result.normalized', 'wp-codebox-fuzz-suite-result'),
					artifact('fuzz.result.envelope', 'result-envelope'),
					artifact('fuzz.case.log', 'case-log'),
					artifact('fuzz.replay.data', 'replay-data'),
					artifact('fuzz.coverage.summary', 'coverage-summary'),
					artifact('fuzz.disposable.sandbox_isolation_proof', 'sandbox-isolation-proof'),
					artifact('fuzz.mutation.isolation', 'mutation-isolation-artifact'),
					artifact('fuzz.delete.boundary', 'delete-boundary-artifact'),
					artifact('fuzz.external_http.guardrail', 'external-http-guardrail'),
					artifact('fuzz.runtime.access', 'runtime-access'),
					artifact('fuzz.coverage', 'wordpress-fuzz-coverage'),
					artifact('fuzz.hotspot.summary', 'homeboy-hotspot-summary'),
					artifact('fuzz.coverage.gap_report', 'wordpress-fuzz-gap-report'),
					artifact('fuzz.hotspot.codebox', 'wordpress-hotspots', 'wp-codebox/wordpress-hotspots/v1'),
				],
			};
		},
	});

	assert.equal(summary.schema, WORDPRESS_FUZZ_CAMPAIGN_RUN_SCHEMA);
	assert.equal(summary.status, 'succeeded');
	assert.equal(summary.succeeded, true);
	assert.equal(summary.campaign.wp_codebox.input.schema, 'wp-codebox/fuzz-suite/v1');
	assert.equal(summary.campaign.wp_codebox.input.metadata.production_campaign, true);
	assert.equal(summary.artifact_validation.schema, WORDPRESS_FUZZ_CAMPAIGN_ARTIFACT_VALIDATION_SCHEMA);
	assert.equal(summary.artifact_validation.status, 'passed');
	assert.equal(summary.artifact_validation.required_artifacts.length, 7);
	assert.equal(summary.artifact_validation.missing_artifacts.length, 0);
	assert.equal(seen.request.input.schema, 'wp-codebox/fuzz-suite/v1');
	assert.equal(seen.request.expected_artifacts.includes('wordpress-hotspots'), true);
	assert.equal(seen.request.artifact_declarations.find((entry) => entry.semantic_key === 'fuzz.hotspot.codebox').required, true);
	assert.equal(JSON.parse(fs.readFileSync(summaryPath, 'utf8')).schema, WORDPRESS_FUZZ_CAMPAIGN_RUN_SCHEMA);
	assert(!JSON.stringify(summary).includes('woocommerce'), 'campaign orchestrator must stay product-agnostic');

	const missing = validateWordPressFuzzCampaignArtifacts({
		destructive: true,
		result: { artifacts: [artifact('fuzz.coverage')] },
	});
	assert.equal(missing.status, 'failed');
	assert(missing.missing_artifacts.some((entry) => entry.semantic_key === 'fuzz.delete.boundary'));

	console.log('wordpress fuzz campaign orchestrator smoke passed');
})().catch((error) => {
	console.error(error);
	process.exitCode = 1;
});
