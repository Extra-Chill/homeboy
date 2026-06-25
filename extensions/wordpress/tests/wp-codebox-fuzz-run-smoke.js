'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS,
	DEFAULT_FUZZ_SUITE_ABILITY,
	DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS,
	DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY,
	DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA,
	ARTIFACT_POSTPROCESS_COMMAND,
	WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA,
	WP_CODEBOX_FUZZ_PREFLIGHT_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_SCHEMA,
	buildWordPressFuzzCommandManifest,
	wpCodeboxFuzzSuiteAbility,
	wpCodeboxFuzzSuiteSchema,
	wpCodeboxWordPressWorkloadRunAbility,
	wpCodeboxWordPressWorkloadRunInput,
	wpCodeboxWordPressWorkloadRunSchema,
	normalizeWpCodeboxFuzzSuiteResult,
	detectWpCodeboxPublicFuzzCapabilities,
	preflightWpCodeboxFuzzCapabilityContract,
	runWpCodeboxFuzzSuite,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzSuiteTaskRequest,
} = require('../lib/wp-codebox-fuzz-run');

const input = wpCodeboxFuzzSuiteInput({
	id: 'fuzz-smoke',
	target: { type: 'wordpress-plugin', slug: 'sample-plugin' },
	workload: { entry: 'rest-routes' },
	cases: [{ method: 'GET', path: '/wp/v2/posts' }],
	seeds: [{ name: 'sample-post' }],
	limits: { max_cases: 1 },
	coverage: { hooks: true, db: true },
	runtimeProfile: { components: [{ name: 'sample-plugin', path: '/workspace/sample-plugin' }] },
	metadata: { scenario: 'smoke' },
});

assert.equal(input.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(input.target.slug, 'sample-plugin');
assert.deepEqual(input.metadata.limits, { max_cases: 1 });
assert.equal(wpCodeboxFuzzSuiteInput({ id: 'suite-alias' }).schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);

const manifest = {
	schema: 'wp-codebox/runtime-contract-manifest/v1',
	version: 1,
	schemas: {
		wordpressRuntime: {
			workloadRun: 'wp-codebox/wordpress-workload-run/v1',
			fuzzSuite: 'wp-codebox/fuzz-suite/v1',
			fuzzSuiteResult: 'wp-codebox/fuzz-suite-result/v1',
		},
	},
	abilities: {
		wordpressRuntime: {
			runWorkload: 'wp-codebox/run-wordpress-workload',
			runFuzzSuite: 'wp-codebox/run-fuzz-suite',
		},
	},
};

assert.equal(wpCodeboxFuzzSuiteAbility({ runtimeContractManifest: manifest }), DEFAULT_FUZZ_SUITE_ABILITY);
assert.equal(wpCodeboxFuzzSuiteSchema({ runtimeContractManifest: manifest }), WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(wpCodeboxWordPressWorkloadRunAbility({ runtimeContractManifest: manifest }), DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY);
assert.equal(wpCodeboxWordPressWorkloadRunSchema({ runtimeContractManifest: manifest }), DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA);
assert.deepEqual(wpCodeboxWordPressWorkloadRunInput({
	id: 'workload-run',
	steps: [{ command: 'wordpress.run-declarative-fuzz' }],
	metadata: { source: 'smoke' },
}), {
	schema: DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA,
	id: 'workload-run',
	mounts: [],
	runtime_stack_mounts: [],
	runtime_overlays: [],
	secret_env: [],
	staged_files: [],
	before: [],
	steps: [{ command: 'wordpress.run-declarative-fuzz' }],
	after: [],
	metadata: { source: 'smoke' },
});

const artifactPostprocessWorkloadInput = wpCodeboxWordPressWorkloadRunInput({
	id: 'artifact-postprocess-workload-run',
	steps: [{
		command: 'artifact-postprocess',
		args: {
			helper: '${package.root}/tools/artifact-helper.mjs',
			action: 'coverage-gap-report',
			input: { type: 'artifact-root', path: '${artifacts.root}' },
			output: { artifact: 'coverage_gap_report', path: 'coverage/gaps.json', semantic_key: 'fuzz.coverage.gap_report' },
			parameters: { max_bytes: 1024 },
		},
	}],
});
assert.equal(artifactPostprocessWorkloadInput.steps[0].command, ARTIFACT_POSTPROCESS_COMMAND);
assert.equal(artifactPostprocessWorkloadInput.steps[0].args.helper, '${package.root}/tools/artifact-helper.mjs');
assert.equal(artifactPostprocessWorkloadInput.steps[0].args.action, 'coverage-gap-report');
assert.equal(artifactPostprocessWorkloadInput.steps[0].args.output.semantic_key, 'fuzz.coverage.gap_report');
assert.equal(artifactPostprocessWorkloadInput.steps[0].metadata.contract, 'homeboy/artifact-postprocess/v1');

const taskRequest = wpCodeboxFuzzSuiteTaskRequest({
	taskId: 'wp-codebox-fuzz-suite-smoke',
	input,
	provider: 'codex',
	runtimeId: 'wp-codebox',
});

assert.equal(taskRequest.executor.backend, 'codebox');
assert.equal(taskRequest.executor.runtime, 'wp-codebox');
assert.equal(taskRequest.executor.config.runtime_task.ability, DEFAULT_FUZZ_SUITE_ABILITY);
assert.equal(taskRequest.executor.config.runtime_task.input.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.deepEqual(taskRequest.expected_artifacts, DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS);
assert.deepEqual(taskRequest.artifact_declarations, DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS);
assert.deepEqual(
	taskRequest.artifact_declarations.filter((artifact) => ['result-envelope', 'case-log', 'replay-data', 'coverage-summary'].includes(artifact.name)).map((artifact) => [artifact.name, artifact.semantic_key, artifact.required]),
	[
		['result-envelope', 'fuzz.result.envelope', true],
		['case-log', 'fuzz.case.log', true],
		['replay-data', 'fuzz.replay.data', true],
		['coverage-summary', 'fuzz.coverage.summary', true],
	]
);
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'fuzz-observation-set').role, 'observation_set');
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'wp-codebox-fuzz-suite-result').role, 'codebox_result');
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'case-log').role, 'case_log');
assert.deepEqual(
	taskRequest.artifact_declarations.filter((artifact) => artifact.required === true).map((artifact) => artifact.name),
	taskRequest.expected_artifacts
);
assert(!JSON.stringify(taskRequest).includes('woocommerce'), 'fuzz suite helper must stay product-agnostic');
assert.equal(wpCodeboxFuzzSuiteTaskRequest({ taskId: 'suite-task' }).executor.config.runtime_task.input.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);

const preflightMissingCommand = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	runtimeContractManifest: manifest,
	publicCliCapabilities: { commands: { 'run-wordpress-workload': true } },
});
assert.equal(preflightMissingCommand.schema, WP_CODEBOX_FUZZ_PREFLIGHT_SCHEMA);
assert.equal(preflightMissingCommand.ok, false);
assert.deepEqual(preflightMissingCommand.missing_contracts.map((contract) => contract.command).filter(Boolean), ['run-fuzz-suite']);
assert.equal(preflightMissingCommand.diagnostics[0].code, 'wp_codebox_fuzz_missing_public_cli_command');
assert.equal(preflightMissingCommand.command_manifest.schema, 'homeboy/wordpress-fuzz-command-manifest/v1');
assert.deepEqual(preflightMissingCommand.command_manifest.case_intents['request-rest-route'].commands, ['run-wordpress-workload']);

const commandManifest = buildWordPressFuzzCommandManifest();
assert.deepEqual(commandManifest.wp_codebox.public_commands, ['run-fuzz-suite', 'run-wordpress-workload']);
assert.equal(commandManifest.wp_codebox.abilities.runWorkload, DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY);

const preflightMissingAbility = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	runtimeContractManifest: { schema: manifest.schema, abilities: { wordpressRuntime: { runFuzzSuite: DEFAULT_FUZZ_SUITE_ABILITY } } },
	publicCliCapabilities: { commands: { 'run-fuzz-suite': true, 'run-wordpress-workload': true } },
});
assert.equal(preflightMissingAbility.ok, false);
assert.equal(preflightMissingAbility.missing_contracts.some((contract) => contract.ability === DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY), true);

const preflightPassed = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	runtimeContractManifest: manifest,
	publicCliCapabilities: { commands: { 'run-fuzz-suite': true, 'run-wordpress-workload': true } },
});
assert.equal(preflightPassed.ok, true);

const tempWorkloadDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-fuzz-run-smoke-'));
const jsonWorkloadPath = path.join(tempWorkloadDir, 'json-workload-smoke.workload.json');
fs.writeFileSync(jsonWorkloadPath, `${JSON.stringify({
	id: 'json-workload-smoke',
	run: [{ type: 'php', code: 'return array("ok" => true);' }],
	metadata: { fixture: 'json-workload-smoke' },
})}\n`, 'utf8');

const jsonWorkloadManifest = {
	schema: 'homeboy/fuzz-workload/v1',
	id: 'json-workload-smoke',
	label: 'JSON workload smoke',
	target: { type: 'wordpress-plugin', slug: 'sample-plugin' },
	workload: {
		runner: 'wp-codebox',
		type: 'json',
		path: jsonWorkloadPath,
		entry: 'wp-codebox/run-fuzz-suite',
	},
	artifacts: {
		expected: [{ name: 'json_fuzz_result', role: 'fuzz_report', semantic_key: 'fuzz.suite_result', schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA, required: true }],
	},
	cases: [{
		case_id: 'json-workload-smoke:default',
		artifacts: [{ name: 'json_fuzz_result', path: 'json-workload-smoke/fuzz-suite-result.json', required: true }],
		intent: {
			schema: 'homeboy/fuzz-workload-intent/v1',
			type: 'wordpress-plugin-workload',
			plugin: { activation: 'sample-plugin/sample-plugin.php' },
			execute: { workload_ref: 'default', path: jsonWorkloadPath, type: 'json', entry: 'wp-codebox/run-fuzz-suite' },
			collect: [{ artifact: 'json_fuzz_result' }],
		},
	}],
};
const jsonWorkloadInput = wpCodeboxFuzzSuiteInput({ id: 'json-workload-run', homeboyFuzzWorkload: jsonWorkloadManifest });
assert.equal(jsonWorkloadInput.cases.length, 1);
assert.equal(jsonWorkloadInput.cases[0].id, 'json-workload-smoke:default');
assert.equal(jsonWorkloadInput.cases[0].target.kind, 'runtime');
assert.equal(jsonWorkloadInput.cases[0].target.entrypoint, 'wordpress.run-workload');
assert.deepEqual(jsonWorkloadInput.cases[0].input, {
	schema: DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA,
	id: 'json-workload-smoke',
	mounts: [],
	runtime_stack_mounts: [],
	runtime_overlays: [],
	secret_env: [],
	staged_files: [],
	before: [],
	steps: [{ type: 'php', code: 'return array("ok" => true);' }],
	after: [],
	metadata: { fixture: 'json-workload-smoke', source_path: jsonWorkloadPath, source_entry: 'wp-codebox/run-fuzz-suite' },
});
assert.deepEqual(jsonWorkloadInput.cases[0].phases.setup, [{ command: 'wordpress.wp-cli', args: ['command=plugin activate sample-plugin/sample-plugin.php'] }]);
assert.deepEqual(jsonWorkloadInput.cases[0].phases.action, [{ command: 'wordpress.run-workload', args: [`path=${jsonWorkloadPath}`] }]);
assert.deepEqual(jsonWorkloadInput.cases[0].phases.assert, [{ command: 'wordpress.collect-workload-result', args: ['artifact=json_fuzz_result'] }]);
assert.equal(jsonWorkloadInput.cases[0].artifacts[0].required, true);
assert.equal(jsonWorkloadInput.cases[0].artifacts[0].metadata.semantic_key, 'fuzz.suite_result');
assert.equal(jsonWorkloadInput.metadata.artifacts.expected[0].required, true);

const wooDbApiWorkloadPath = path.join(tempWorkloadDir, 'rest-db-query-profile.workload.json');
fs.writeFileSync(wooDbApiWorkloadPath, `${JSON.stringify({
	id: 'rest-db-query-profile',
	run: [
		{ type: 'php', code: 'return array("loaded" => true);' },
		{ type: 'rest-db-query-profiler', 'metric-prefix': 'rest_db_query_profile', sampleLimit: 50 },
	],
	metadata: { runner: 'wp-codebox', workload: 'rest-db-query-profile' },
})}\n`, 'utf8');
const wooDbApiFixture = JSON.parse(fs.readFileSync(path.join(__dirname, 'fixtures/woo-db-api-rest-query-profile-fuzz.json'), 'utf8'));
wooDbApiFixture.workload.workload.path = wooDbApiWorkloadPath;
wooDbApiFixture.workload.cases[0].intent.execute.path = wooDbApiWorkloadPath;
const wooDbApiInput = wpCodeboxFuzzSuiteInput({ id: 'woo-db-api-rest-query-profile-run', homeboyFuzzWorkload: wooDbApiFixture.workload });
assert.equal(wooDbApiInput.cases[0].id, 'rest-db-query-profile:default');
assert.deepEqual(wooDbApiInput.cases[0].target, { kind: 'runtime', id: 'wordpress.run-workload', entrypoint: 'wordpress.run-workload' });
assert.equal(wooDbApiInput.cases[0].input.schema, DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA);
assert.deepEqual(wooDbApiInput.cases[0].input.steps, [
	{ type: 'php', code: 'return array("loaded" => true);' },
	{ type: 'rest-db-query-profiler', 'metric-prefix': 'rest_db_query_profile', sampleLimit: 50 },
]);
assert.deepEqual(wooDbApiInput.cases[0].phases.action, [{ command: 'wordpress.run-workload', args: [`path=${wooDbApiWorkloadPath}`] }]);
const wooDbApiSummary = normalizeWpCodeboxFuzzSuiteResult(wooDbApiFixture.result);
assert.equal(wooDbApiSummary.hotspot_summary.items[0].value, 12);
assert.equal(wooDbApiSummary.observation_set.observations[0].fingerprint, 'select-products');
assert.equal(wooDbApiSummary.observation_set.observations[1].metric, 'duration_ms');

const genericPrimitiveManifest = {
	schema: 'homeboy/fuzz-workload/v1',
	id: 'generic-primitive-smoke',
	label: 'Generic primitive smoke',
	metadata: {
		generic_primitive: { command: 'wordpress.fuzz-admin-pages', status: 'preferred' },
	},
	workload: {
		runner: 'wp-codebox',
		type: 'php',
		path: '${package.root}/bench/admin-page-coverage.php',
		entry: 'admin-page-coverage',
	},
	cases: [{
		case_id: 'generic-primitive-smoke:default',
		artifacts: [{ name: 'admin_page_coverage', path: 'admin-page-coverage/admin_page_coverage.json', required: true }],
		intent: {
			schema: 'homeboy/fuzz-workload-intent/v1',
			type: 'wordpress-plugin-workload',
			plugin: { activation: 'sample-plugin/sample-plugin.php' },
			execute: {
				path: '${package.root}/bench/admin-page-coverage.php',
				type: 'php',
				parameters: { safe_methods: 'GET', max_pages: '80', enumerate_menus: 'true' },
			},
			collect: [{ artifact: 'admin_page_coverage' }],
		},
	}],
};
const genericPrimitiveInput = wpCodeboxFuzzSuiteInput({ id: 'generic-primitive-run', homeboyFuzzWorkload: genericPrimitiveManifest });
assert.equal(genericPrimitiveInput.cases[0].target.entrypoint, 'wordpress.fuzz-admin-pages');
assert.deepEqual(genericPrimitiveInput.cases[0].phases.setup, [{ command: 'wordpress.wp-cli', args: ['command=plugin activate sample-plugin/sample-plugin.php'] }]);
assert.deepEqual(genericPrimitiveInput.cases[0].phases.action, [{ command: 'wordpress.fuzz-admin-pages', args: ['safe_methods=GET', 'max_pages=80', 'enumerate_menus=true'] }]);
assert.deepEqual(genericPrimitiveInput.cases[0].phases.assert, [{ command: 'wordpress.collect-workload-result', args: ['artifact=admin_page_coverage'] }]);

const planWorkloadManifest = {
	schema: 'homeboy/fuzz-workload/v1',
	id: 'plan-workload-smoke',
	label: 'Plan workload smoke',
	target: { type: 'wordpress-plugin', slug: 'sample-plugin', component: 'sample-plugin' },
	metadata: {
		fixture: { component: 'sample-plugin', activation: 'sample-plugin/sample-plugin.php' },
	},
	plan: {
		schema: 'wordpress-fuzz-plan/v1',
		id: 'plan-workload-smoke',
		targets: [{
			id: 'sample-rest-routes',
			surface_id: 'sample-rest-routes',
			cases: [{
				id: 'plan-workload-smoke:default',
				command: 'wordpress.inventory-rest-routes',
				input: {
					plugin: 'sample-plugin/sample-plugin.php',
					namespaces: ['sample/v1', 'sample/v2'],
					artifact: 'route_inventory',
				},
				inputs: {
					observation_surfaces: ['rest_generated_cases'],
					budget_keys: ['max_rest_p95_duration_ms'],
				},
				metadata: { expected_artifact: 'route_inventory' },
			}],
		}],
	},
	artifacts: {
		expected: [{ name: 'route_inventory', role: 'fuzz_report', semantic_key: 'fuzz.report', required: true }],
	},
	cases: [{
		case_id: 'legacy-intent-should-not-win',
		intent: {
			plugin: { activation: 'sample-plugin/sample-plugin.php' },
			execute: { path: '/host-only/workload.php', type: 'php' },
		},
	}],
};
const planWorkloadInput = wpCodeboxFuzzSuiteInput({ id: 'plan-workload-run', homeboyFuzzWorkload: planWorkloadManifest });
assert.equal(planWorkloadInput.cases.length, 1);
assert.equal(planWorkloadInput.cases[0].id, 'plan-workload-smoke:default');
assert.equal(planWorkloadInput.cases[0].target.entrypoint, 'wordpress.inventory-rest-routes');
assert.deepEqual(planWorkloadInput.cases[0].input, { args: ['plugin=sample-plugin/sample-plugin.php', 'namespaces=sample/v1,sample/v2', 'artifact=route_inventory'] });
assert.deepEqual(planWorkloadInput.cases[0].phases.setup, [{ command: 'wordpress.wp-cli', args: ['command=plugin activate sample-plugin/sample-plugin.php'] }]);
assert.deepEqual(planWorkloadInput.cases[0].phases.action, [{
	command: 'wordpress.inventory-rest-routes',
	args: ['plugin=sample-plugin/sample-plugin.php', 'namespaces=sample/v1,sample/v2', 'artifact=route_inventory'],
}]);
assert.equal(JSON.stringify(planWorkloadInput).includes('/host-only/workload.php'), false);
assert.equal(planWorkloadInput.cases[0].metadata.source_plan_case, true);
assert.equal(planWorkloadInput.cases[0].metadata.target_id, 'sample-rest-routes');
assert.deepEqual(planWorkloadInput.cases[0].inputs.budget_keys, ['max_rest_p95_duration_ms']);

let invoked = false;
runWpCodeboxFuzzSuite({
	taskId: 'wp-codebox-fuzz-suite-delegation-smoke',
	input,
	runFuzzSuite: async (request) => {
		invoked = true;
		assert.equal(request.executor.config.runtime_task.input.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
		return {
			json: {
				schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
				suite: { id: 'fuzz-smoke' },
				request_id: request.task_id,
				status: 'succeeded',
				summary: { total: 2, passed: 1, failed: 0, error: 0, skipped: 1 },
				coverage_summary: {
					surface_count: 3,
					exercised_count: 1,
					skipped_count: 1,
					failed_count: 1,
				},
				coverage_gaps: [{ id: 'route:/wp/v2/users', type: 'rest_route', status: 'skipped' }],
				coverage: { hooks: { actions: { init: 1 } } },
				queries: [{ case_id: 'case-000', target_id: 'target-rest', operation_id: 'GET /wp/v2/posts', query: 'SELECT * FROM wp_posts', metric: 'query_count', count: 4, fingerprint: 'select-posts' }],
				timings: [{ case_id: 'case-000', target_id: 'target-rest', operation_id: 'GET /wp/v2/posts', subject: 'request', duration_ms: 99 }],
				wordpress_fuzz_result: {
					schema: 'wordpress-fuzz-result/v1',
					id: 'normalized-result',
					plan_id: 'generic-plan',
					status: 'passed',
					cases: [
						{
							id: 'case-000',
							target_id: 'target-rest',
							surface_id: 'surface-rest',
							operation_id: 'rest:list-posts',
							status: 'passed',
							role_boundary: { role: 'subscriber', outcome: 'allowed_as_expected' },
							db_query: { query_count: 1, rows_examined: 2, duration_ms: 3 },
							http_guardrail: { blocked: 1 },
						},
						{
							id: 'case-001',
							target_id: 'target-admin',
							surface_id: 'surface-admin',
							operation_id: 'admin:settings',
							status: 'skipped',
							skip_reason: 'capability_unavailable',
							destructive_reason: 'mutating_action',
							admin_browser: { errors: [{ message: 'blocked navigation' }] },
						},
					],
					provenance: { workload_manifest: 'workloads/generic-wordpress-fuzz.json' },
				},
				artifacts: {
					fuzz_report: { path: 'reports/fuzz-report.json', content_type: 'application/json' },
					coverage: { path: 'reports/coverage.json', content_type: 'application/json', size_bytes: 123, payload: { schema: 'wp-codebox/coverage-report/v1', covered: 1 } },
					normalized_fuzz_result: { path: 'reports/wordpress-fuzz-result.json', content_type: 'application/json' },
					coverage_gap_report: { path: 'reports/coverage-gaps.json', content_type: 'application/json', schema: 'homeboy/wordpress-coverage-gap-report/v1', semantic_key: 'fuzz.coverage.gap_report', payload: { schema: 'homeboy/wordpress-coverage-gap-report/v1', expected: 2, covered: 1, gaps: [{ id: 'route:/wp/v2/comments', type: 'rest_route', status: 'skipped' }] } },
					hotspot_summary: { path: 'reports/hotspots.json', content_type: 'application/json', semantic_key: 'fuzz.hotspot.summary', payload: { schema: 'homeboy/fuzz-hotspot-summary/v1', metric: 'duration_ms', unit: 'ms', items: [{ surface: 'route:/wp/v2/posts', operation: 'GET /wp/v2/posts', value: 99, rank: 1 }] } },
					fuzz_case: { path: 'cases/case-000.json', case_id: 'case-000' },
					placeholder_case: { name: 'placeholder-only' },
					failing_case: { path: 'cases/failing-case.json', case_id: 'case-002' },
					case_artifact: { path: 'cases/case-001.json', case_id: 'case-001' },
					repro_case: { path: 'repro/case-002.js', case_id: 'case-002' },
				},
				artifactRefs: [
					{ path: 'replay/case-001.json', kind: 'replay', contentType: 'application/json' },
				],
			},
		};
	},
}).then((summary) => {
	assert.equal(invoked, true);
	assert.equal(summary.schema, WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA);
	assert.equal(summary.delegated_schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
	assert.equal(summary.result_schema, WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA);
	assert.equal(summary.succeeded, true);
	assert.equal(summary.metadata.suite.id, 'fuzz-smoke');
	assert.equal(summary.metadata.summary.total, 2);
	assert.equal(summary.coverage.hooks.actions.init, 1);
	assert.equal(summary.coverage_summary.surface_count, 3);
	assert.equal(summary.coverage_summary.exercised_count, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.surface_count, 2);
	assert.equal(summary.wordpress_fuzz_result.summary.operation_count, 2);
	assert.equal(summary.wordpress_fuzz_result.summary.skipped_reason_codes.capability_unavailable, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.destructive_reason_codes.mutating_action, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.role_boundary_outcomes.by_outcome.allowed_as_expected, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.db_query_metrics.query_count, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.admin_browser_errors.errors, 1);
	assert.equal(summary.wordpress_fuzz_result.summary.http_guardrail_outcomes.blocked, 1);
	assert.equal(summary.wordpress_fuzz_result.provenance.workload_manifest, 'workloads/generic-wordpress-fuzz.json');
	assert.equal(summary.wordpress_fuzz_result.artifacts.some((artifact) => artifact.role === 'coverage'), true);
	assert.equal(summary.wordpress_fuzz_result.artifacts.some((artifact) => artifact.name === 'placeholder-only'), false);
	assert.equal(summary.coverage_gaps[0].status, 'skipped');
	assert.equal(summary.coverage_gaps.some((gap) => gap.id === 'route:/wp/v2/comments'), true);
	assert.equal(summary.derived_artifacts.coverage_gap_reports[0].coverage_gaps[0].id, 'route:/wp/v2/comments');
	assert.equal(summary.hotspot_summary.items[0].value, 99);
	assert.equal(summary.observation_set.schema, 'homeboy/fuzz-observation-set/v1');
	assert.equal(summary.observation_set.observations[0].family, 'query');
	assert.equal(summary.observation_set.observations[1].metric, 'duration_ms');
	assert.equal(summary.runtime_task_result.observation_set.observations[0].fingerprint, 'select-posts');
	assert.equal(summary.derived_artifacts.artifacts.some((artifact) => artifact.role === 'hotspot_summary'), true);
	assert.deepEqual(summary.artifacts.map((artifact) => artifact.role), ['fuzz_report', 'coverage', 'normalized_fuzz_result', 'coverage_gap_report', 'hotspot_summary', 'fuzz_case', 'failing_case', 'case_artifact', 'repro_case', 'repro_case']);
	assert.equal(summary.artifacts[0].semantic_key, 'fuzz.report');
	assert.equal(summary.artifacts[9].semantic_key, 'fuzz.case.repro');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'coverage').semantic_key, 'fuzz.coverage');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'coverage').size_bytes, 123);
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'coverage').payload.schema, 'wp-codebox/coverage-report/v1');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'normalized_fuzz_result').semantic_key, 'fuzz.result.normalized');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'fuzz_case').semantic_key, 'fuzz.case');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'fuzz_case').case_id, 'case-000');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'failing_case').semantic_key, 'fuzz.case.failing');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'case_artifact').semantic_key, 'fuzz.case.artifact');
	assert.equal(summary.artifacts.find((artifact) => artifact.role === 'repro_case').semantic_key, 'fuzz.case.repro');
	assert.equal(summary.artifacts.some((artifact) => artifact.name === 'placeholder-only'), false);
	assert.deepEqual(normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			status: 'passed',
			summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
			cases: [{ id: 'artifact-contract-case', status: 'passed' }],
			artifactRefs: [
				{ name: 'case-log', path: 'cases/case-log.jsonl' },
				{ name: 'replay-data', path: 'replay/replay-data.json' },
				{ name: 'coverage-summary', path: 'coverage/summary.json' },
			],
		},
	}).artifacts.map((artifact) => [artifact.role, artifact.semantic_key]), [
		['case_log', 'fuzz.case.log'],
		['replay_data', 'fuzz.replay.data'],
		['coverage_summary', 'fuzz.coverage.summary'],
	]);

	const normalized = normalizeWpCodeboxFuzzSuiteResult({ status: 'failed', failures: [{ message: 'boom' }] });
	assert.equal(normalized.succeeded, false);
	assert.equal(normalized.failures[0].message, 'boom');
	const nested = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: 'wp-codebox/agent-task-run/v1',
			status: 'no_op',
			agent_task_result: {
				result: {
					schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
					status: 'passed',
					suite: { id: 'nested-suite' },
					summary: { total: 0, passed: 0, failed: 0, error: 0, skipped: 0 },
				},
			},
		},
	});
	assert.equal(nested.succeeded, true);
	assert.equal(nested.metadata.suite.id, 'nested-suite');
	const embeddedArtifactOnly = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			status: 'passed',
			suite: { id: 'embedded-artifact-suite' },
			summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
			wordpress_fuzz_result: {
				schema: 'wordpress-fuzz-result/v1',
				status: 'passed',
				cases: [{
					id: 'case-with-artifact-ref',
					status: 'passed',
					artifactRefs: [{ name: 'case_report', path: 'case/report.json', kind: 'fuzz_report', contentType: 'application/json' }],
				}],
			},
		},
	}, { request: taskRequest });
	assert.equal(embeddedArtifactOnly.succeeded, true);
	assert.equal(embeddedArtifactOnly.artifacts[0].path, 'case/report.json');
	assert.equal(embeddedArtifactOnly.artifacts[0].role, 'fuzz_report');
	assert.equal(embeddedArtifactOnly.artifacts[0].semantic_key, 'fuzz.report');
	assert.equal(embeddedArtifactOnly.artifacts[0].name, 'case_report');
	assert.equal(embeddedArtifactOnly.wordpress_fuzz_result.artifacts[0].role, 'fuzz_report');
	assert.equal(embeddedArtifactOnly.wordpress_fuzz_result.artifacts[0].name, 'case_report');
	const doubleNested = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: 'wp-codebox/agent-task-run/v1',
			status: 'no_op',
			agent_result: {
				result: {
					result: {
						schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
						status: 'passed',
						suite: { id: 'double-nested-suite' },
						summary: { total: 0, passed: 0, failed: 0, error: 0, skipped: 0 },
					},
				},
			},
		},
	});
	assert.equal(doubleNested.succeeded, true);
	assert.equal(doubleNested.metadata.suite.id, 'double-nested-suite');
	const rawNested = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: 'wp-codebox/agent-task-run/v1',
			status: 'no_op',
			agent_task_result: {
				raw: {
					result: {
						schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
						status: 'passed',
						suite: { id: 'raw-nested-suite' },
						summary: { total: 0, passed: 0, failed: 0, error: 0, skipped: 0 },
					},
				},
			},
		},
	});
	assert.equal(rawNested.succeeded, true);
	assert.equal(rawNested.metadata.suite.id, 'raw-nested-suite');
	const emptyRequired = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			status: 'passed',
			suite: { id: 'empty-required-suite' },
			summary: { total: 0, passed: 0, failed: 0, error: 0, skipped: 0 },
		},
	}, { request: taskRequest });
	assert.equal(emptyRequired.succeeded, false);
	assert.deepEqual(emptyRequired.failures.map((failure) => failure.code), [
		'wp_codebox_fuzz_empty_cases_for_declared_contract',
		'wp_codebox_fuzz_required_artifacts_missing',
	]);
	const declaredOnlyEmpty = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			status: 'passed',
			suite: { id: 'declared-only-suite' },
			summary: { total: 0, passed: 0, failed: 0, error: 0, skipped: 0 },
			metadata: { readiness: { level: 'declared' } },
		},
	}, { request: taskRequest });
	assert.equal(declaredOnlyEmpty.succeeded, true);
	assert.deepEqual(declaredOnlyEmpty.failures, []);
	assert.equal(normalizeWpCodeboxFuzzSuiteResult({ status: 'passed' }).result_schema, WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA);
	assert.equal(normalizeWpCodeboxFuzzSuiteResult({ status: 'passed' }).schema, WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA);
	return runWpCodeboxFuzzSuite({ taskId: 'suite-run', runFuzzSuite: async () => ({ status: 'passed' }) });
}).then((summary) => {
	assert.equal(summary.delegated_schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
	return runWpCodeboxFuzzSuite({
		taskId: 'public-cli-suite-run',
		input,
		runPublicCli: ({ args, stdin }) => {
			if (args.join(' ') === 'run-fuzz-suite --help') {
				return { status: 0, stdout: 'usage' };
			}
			if (args.join(' ') === 'run-wordpress-workload --help') {
				return { status: 0, stdout: 'usage' };
			}
			assert.equal(args[0], 'run-fuzz-suite');
			assert.equal(args[1], '--input-file');
			assert.equal(args[3], '--format=json');
			assert.equal(stdin, undefined);
			assert.equal(JSON.parse(fs.readFileSync(args[2], 'utf8')).schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
			return {
				status: 0,
				stdout: JSON.stringify({
					schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
					status: 'succeeded',
					summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
					cases: [{ id: 'public-cli-case', status: 'passed' }],
					artifactRefs: [
						{ name: 'wp-codebox-fuzz-suite-result', path: 'result.json' },
						{ name: 'wordpress-fuzz-coverage', path: 'coverage.json' },
						{ name: 'result-envelope', path: 'envelope.json' },
						{ name: 'case-log', path: 'cases.jsonl' },
						{ name: 'replay-data', path: 'replay.json' },
						{ name: 'coverage-summary', path: 'summary.json' },
					],
				}),
			};
		},
	});
}).then((summary) => {
	assert.equal(summary.succeeded, true);
	assert.equal(summary.artifacts.some((artifact) => artifact.name === 'case-log'), true);
	assert.deepEqual(detectWpCodeboxPublicFuzzCapabilities({ publicCliCapabilities: { commands: { 'run-wordpress-workload': true } } }).commands, {
		'run-fuzz-suite': false,
		'run-wordpress-workload': true,
	});
	return runWpCodeboxFuzzSuite({
		taskId: 'public-cli-unsupported-run',
		input,
		runPublicCli: () => ({ status: 1, stderr: 'unknown command' }),
	});
}).then((summary) => {
	assert.equal(summary.succeeded, false);
	assert.equal(summary.failures[0].code, 'wp_codebox_fuzz_missing_public_cli_command');
	assert.equal(summary.failures.some((failure) => failure.code === 'wp_codebox_fuzz_required_artifacts_missing'), false);

	console.log('wp-codebox fuzz-run smoke passed');
}).catch((error) => {
	console.error(error);
	process.exit(1);
});
