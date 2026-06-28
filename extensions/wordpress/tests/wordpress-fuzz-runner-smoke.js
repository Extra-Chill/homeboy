'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const Module = require('node:module');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const manifest = require('../wordpress.json');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(
	__dirname,
	'..',
	'..',
	'tests',
	'fixtures',
	'wp-codebox-core-runtime-contract.cjs'
);

const originalLoad = Module._load;
Module._load = function loadWithoutRuntimeAgentCi(request, parent, isMain) {
	if (request.includes('runtime-agent-ci')) {
		throw new Error(`WordPress fuzz runner must be installable without runtime-agent-ci: ${request}`);
	}
	return originalLoad.call(this, request, parent, isMain);
};

const {
	HOMEBOY_FUZZ_CAMPAIGN_SCHEMA,
	HOMEBOY_FUZZ_RESULT_ENVELOPE_SCHEMA,
	WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
	buildWordPressFuzzRunnerResult,
	readWordPressFuzzRunnerEnv,
	runWordPressFuzzRunnerResult,
	writeHomeboyFuzzArtifactFiles,
} = require('../lib/wordpress-fuzz-runner');

Module._load = originalLoad;

const workload = {
	id: 'generic-wordpress-workload',
	plan: {
		schema: 'wordpress-fuzz-plan/v1',
		id: 'generic-wordpress-plan',
		targets: [
			{
				id: 'rest-posts',
				surface_id: 'route:/wp/v2/posts',
				cases: [{ id: 'get-posts', method: 'GET', path: '/wp/v2/posts' }],
			},
		],
	},
	wp_codebox_plan: {
		id: 'codebox-plan',
		cases: [
			{
				case_id: 'get-posts',
				action: [{ command: 'wp-rest-request', args: ['GET', '/wp/v2/posts'] }],
			},
		],
	},
	coverage_artifacts: [
		{
			schema: 'homeboy/wordpress-fuzz-coverage/v1',
			hooks: { actions: { init: 1 } },
		},
	],
};

const result = buildWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		workloadId: 'workload-from-env',
		runId: 'run-from-env',
		seed: 'seed-123',
		maxDuration: '30',
	},
	workload,
});

assert.equal(result.schema, WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA);
assert.equal(result.status, 'unsupported');
assert.equal(result.succeeded, false);
assert.equal(result.run_id, 'run-from-env');
assert.equal(result.workload_id, 'workload-from-env');
assert.equal(result.seed, 'seed-123');
assert.equal(result.max_duration_seconds, 30);
assert.equal(result.wp_codebox_input.schema, 'wp-codebox/fuzz-suite/v1');
assert.equal(result.wp_codebox_input.goal, 'Run WordPress fuzz suite workload-from-env and return the declared fuzz artifacts.');
assert.equal(result.wp_codebox_input.cases[0].target_id, 'rest-posts');
assert.equal(result.fuzz_runtime_task_request.schema, 'homeboy/fuzz-runtime-task/v1');
assert.equal(result.fuzz_runtime_task_request.provider.id, 'wp-codebox');
assert.equal(result.fuzz_runtime_task_request.provider_request.executor.config.runtime_task.ability, 'wp-codebox/run-fuzz-suite');
assert.equal(result.wp_codebox_task_request.executor.config.runtime_task.ability, 'wp-codebox/run-fuzz-suite');
assert.equal(result.wp_codebox_plan_recipe.fuzzRun, undefined);
assert.equal(result.wp_codebox_plan_recipe.fuzzSuite.cases[0].case_id, 'get-posts');
assert.equal(result.coverage.schema, 'homeboy/wordpress-fuzz-coverage-aggregate/v1');
assert.equal(result.coverage.totals.exercised, 1);
assert.equal(result.homeboy_fuzz_campaign.schema, HOMEBOY_FUZZ_CAMPAIGN_SCHEMA);
assert.equal(result.homeboy_fuzz_campaign.version, 1);
assert.equal(result.homeboy_fuzz_campaign.id, 'run-from-env');
assert.equal(result.homeboy_fuzz_campaign.safety_class, 'read_only');
assert.equal(result.homeboy_fuzz_campaign.metadata.status, 'unsupported');
assert.equal(result.homeboy_fuzz_campaign.metadata.wp_codebox_result_schema, 'wp-codebox/fuzz-suite-result/v1');
assert.equal(result.homeboy_fuzz_campaign.metadata.diagnostics[0].code, 'wp_codebox_fuzz_suite_execution_unsupported');
assert.equal(result.homeboy_fuzz_result_envelope.schema, HOMEBOY_FUZZ_RESULT_ENVELOPE_SCHEMA);
assert.equal(result.homeboy_fuzz_result_envelope.version, 1);
assert.equal(result.homeboy_fuzz_result_envelope.id, 'run-from-env');
assert.equal(result.homeboy_fuzz_result_envelope.campaign.id, 'run-from-env');
assert.equal(result.homeboy_fuzz_result_envelope.campaign.workload_id, 'workload-from-env');
assert.equal(result.homeboy_fuzz_result_envelope.campaign.plan_id, 'generic-wordpress-plan');
assert.equal(result.homeboy_fuzz_result_envelope.campaign.seed, 'seed-123');
assert.equal(result.homeboy_fuzz_result_envelope.campaign.max_duration_seconds, 30);
assert.equal(result.homeboy_fuzz_result_envelope.gates.required_artifacts.some((artifact) => artifact.name === 'result-envelope' && artifact.status === 'present'), true);
assert.equal(result.homeboy_fuzz_result_envelope.gates.required_artifacts.some((artifact) => artifact.name === 'wordpress-fuzz-coverage' && artifact.status === 'missing'), true);
assert.equal(result.homeboy_fuzz_result_envelope.dispatch.task_id, 'run-from-env');
assert.equal(result.homeboy_fuzz_result_envelope.dispatch_identity, undefined);
assert.equal(result.homeboy_fuzz_campaign.metadata.fuzz_result_envelope.schema, HOMEBOY_FUZZ_RESULT_ENVELOPE_SCHEMA);
assert.equal(result.observation.schema, 'homeboy/wordpress-fuzz-observation/v1');
assert.equal(result.homeboy_fuzz_campaign.metadata.observation.schema, 'homeboy/wordpress-fuzz-observation/v1');
assert(!JSON.stringify(result).includes('woocommerce'), 'WordPress fuzz runner must stay product-agnostic');

const executedResult = buildWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		runId: 'executed-run',
	},
	workload: {
		...workload,
		metadata: {
			...(workload.metadata || {}),
			dispatch_identity: {
				source: 'homeboy-agent-task',
				dispatch_id: 'dispatch-123',
				conversation_id: 'conversation-456',
			},
		},
		wp_codebox_suite_result: {
			schema: 'wp-codebox/fuzz-suite-result/v1',
			suite: { id: 'generic-wordpress-plan' },
			status: 'succeeded',
			artifactRefs: [
				{ path: 'artifacts/replay.json', kind: 'replay' },
				{ name: 'result-envelope', role: 'result_envelope', content: { status: 'succeeded' } },
			],
		},
	},
});

assert.equal(executedResult.status, 'succeeded');
assert.equal(executedResult.succeeded, true);
assert.equal(executedResult.homeboy_fuzz_campaign.metadata.artifact_refs[0].path, 'artifacts/replay.json');
assert.equal(executedResult.homeboy_fuzz_campaign.artifacts.some((artifact) => artifact.id === 'result-envelope' && artifact.kind === 'result_envelope'), true);
assert.equal(executedResult.homeboy_fuzz_result_envelope.artifacts[0].path, 'artifacts/replay.json');
assert.equal(executedResult.homeboy_fuzz_result_envelope.gates.required_artifacts.find((artifact) => artifact.name === 'result-envelope').status, 'present');
assert.deepEqual(executedResult.homeboy_fuzz_result_envelope.dispatch_identity, {
	source: 'homeboy-agent-task',
	dispatch_id: 'dispatch-123',
	conversation_id: 'conversation-456',
});
assert.equal(executedResult.observation.status, 'succeeded');

const mutatingPlanResult = buildWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		runId: 'mutating-plan-run',
	},
	workload: {
		...workload,
		plan: {
			schema: 'wordpress-fuzz-plan/v1',
			id: 'mutating-plan',
			targets: [{
				id: 'rest-posts-write',
				surface_id: 'route:/wp/v2/posts',
				cases: [{
					id: 'create-post',
					method: 'POST',
					path: '/wp/v2/posts',
					metadata: { safety: { level: 'mutating', mutates: true } },
				}],
			}],
		},
	},
});

assert.equal(mutatingPlanResult.homeboy_fuzz_campaign.safety_class, 'isolated_mutation');

const jsonWorkloadResult = buildWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		workloadId: 'json-workload',
		runId: 'json-workload-run',
		wpCodeboxFuzzWorkloadRoot: '/runner/workloads',
	},
	workload: {
		schema: 'homeboy/fuzz-workload/v1',
		id: 'json-workload',
		label: 'JSON workload smoke',
		target: { type: 'wordpress-plugin', slug: 'sample-plugin', component: 'sample-plugin' },
		metadata: {
			fixture: { component: 'sample-plugin', activation: 'sample-plugin/sample-plugin.php' },
				homeboy_runtime_context: {
					schema: 'homeboy/fuzz-workload-runtime-context/v1',
					rig_id: 'sample-rig',
					components: {
						'sample-plugin': {
							path: '/runner/components/sample-plugin/plugins/sample-plugin',
							branch: 'main',
							extensions: {
								wordpress: {
									wp_codebox_source_root: '~/components/sample-plugin',
									wp_codebox_source_subpath: 'plugins/sample-plugin',
								},
							},
						},
					},
				},
			},
		workload: {
			runner: 'wp-codebox',
			type: 'json',
			path: '${package.root}/bench/json-workload.workload.json',
			entry: 'wp-codebox/run-fuzz-suite',
		},
		artifacts: {
			expected: [{ name: 'json_fuzz_result', role: 'fuzz_report', semantic_key: 'fuzz.suite_result', schema: 'wp-codebox/fuzz-suite-result/v1', required: true }],
		},
		cases: [{
			case_id: 'json-workload:default',
			artifacts: [{ name: 'json_fuzz_result', path: 'json-workload/fuzz-suite-result.json', required: true }],
			intent: {
				schema: 'homeboy/fuzz-workload-intent/v1',
				type: 'wordpress-plugin-workload',
				plugin: { activation: 'sample-plugin/sample-plugin.php' },
				execute: { workload_ref: 'default', path: '${package.root}/bench/json-workload.workload.json', type: 'json', entry: 'wp-codebox/run-fuzz-suite' },
				collect: [{ artifact: 'json_fuzz_result' }],
			},
		}],
	},
});

assert.equal(jsonWorkloadResult.wp_codebox_input.cases.length, 1);
assert.equal(jsonWorkloadResult.wp_codebox_input.cases[0].id, 'json-workload:default');
assert.deepEqual(jsonWorkloadResult.wp_codebox_input.cases[0].phases.setup, [{ command: 'wordpress.plugin-state', args: ['plugin-state-json={"activate":[{"plugin":"sample-plugin/sample-plugin.php"}],"deactivate":[],"report":true}'] }]);
assert.equal(JSON.stringify(jsonWorkloadResult.wp_codebox_input).includes('wordpress.ensure-plugin-active'), false);
assert.deepEqual(jsonWorkloadResult.wp_codebox_input.cases[0].phases.action, [{ command: 'wordpress.run-workload', args: ['path=${package.root}/bench/json-workload.workload.json'] }]);
assert.deepEqual(jsonWorkloadResult.wp_codebox_input.cases[0].phases.assert, [{ command: 'wordpress.collect-workload-result', args: ['artifact=json_fuzz_result'] }]);
assert.equal(jsonWorkloadResult.wp_codebox_input.cases[0].artifacts[0].required, true);
assert.equal(jsonWorkloadResult.wp_codebox_input.metadata.artifacts.expected[0].semantic_key, 'fuzz.suite_result');
assert.deepEqual(jsonWorkloadResult.wp_codebox_runtime_requirements.extra_plugins, [{
	slug: 'sample-plugin',
	source: '/runner/components/sample-plugin/plugins/sample-plugin',
	sourceRoot: '/runner/components/sample-plugin',
	sourceSubpath: 'plugins/sample-plugin',
	path: '/runner/components/sample-plugin/plugins/sample-plugin',
	pluginFile: 'sample-plugin/sample-plugin.php',
	loadAs: 'plugin',
	metadata: { component: 'sample-plugin', rig_id: 'sample-rig', activation: 'fuzz-suite-setup-step' },
}]);
assert.equal(jsonWorkloadResult.wp_codebox_runtime_requirements.extra_plugins[0].activate, undefined);
assert.equal(
	jsonWorkloadResult.wp_codebox_task_request.executor.config.runtime_requirements.extra_plugins[0].source,
	'/runner/components/sample-plugin/plugins/sample-plugin'
);
assert.equal(jsonWorkloadResult.wp_codebox_task_request.executor.config.runtime_requirements.extra_plugins[0].sourceRoot, '/runner/components/sample-plugin');
assert.equal(jsonWorkloadResult.wp_codebox_task_request.executor.config.runtime_requirements.component_contracts[0].sourceSubpath, 'plugins/sample-plugin');
assert.deepEqual(jsonWorkloadResult.wp_codebox_runtime_requirements.runtime_mounts, [{ source: '/runner/workloads', target: '/runner/workloads', mode: 'readonly' }]);
assert.deepEqual(jsonWorkloadResult.wp_codebox_runtime_requirements.runtime_env, { WP_CODEBOX_FUZZ_WORKLOAD_ROOT: '/runner/workloads' });
assert.ok(manifest.fuzz.env.includes('WP_CODEBOX_FUZZ_WORKLOAD_ROOT'));

const remappedPluginRootResult = buildWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		workloadId: 'jetpack-performance-observation',
		runId: 'jetpack-performance-observation-run',
		wpCodeboxFuzzWorkloadRoot: '/runner/workloads',
	},
	workload: {
		schema: 'homeboy/fuzz-workload/v1',
		id: 'jetpack-performance-observation',
		label: 'Jetpack performance observation',
		target: { type: 'wordpress-plugin', slug: 'jetpack', component: 'jetpack' },
		metadata: {
			fixture: { component: 'jetpack', activation: 'jetpack/jetpack.php' },
			homeboy_runtime_context: {
				schema: 'homeboy/fuzz-workload-runtime-context/v1',
				rig_id: 'jetpack-api-route-inventory',
				components: {
					jetpack: {
						path: '/home/chubes/Developer/_lab_workspaces/jetpack-1234',
						extensions: {
							wordpress: {
								wp_codebox_source_root: '~/Developer/jetpack',
								wp_codebox_source_subpath: 'projects/plugins/jetpack',
							},
						},
					},
				},
			},
		},
		cases: [{
			case_id: 'jetpack-performance-observation:default',
			phases: {
				setup: [{ command: 'wordpress.wp-cli', args: ['command=plugin activate jetpack/jetpack.php'] }],
				action: [{ command: 'wordpress.run-workload', args: ['path=${package.root}/bench/performance.workload.json'] }],
			},
		}],
	},
});
const remappedPlugin = remappedPluginRootResult.wp_codebox_runtime_requirements.extra_plugins[0];
assert.equal(remappedPlugin.source, '/home/chubes/Developer/_lab_workspaces/jetpack-1234');
assert.equal(remappedPlugin.sourceRoot, undefined);
assert.equal(remappedPlugin.sourceSubpath, undefined);
assert.equal(remappedPlugin.pluginFile, 'jetpack/jetpack.php');
assert.equal(remappedPluginRootResult.wp_codebox_runtime_requirements.component_contracts[0].sourceRoot, undefined);
assert.equal(remappedPluginRootResult.wp_codebox_runtime_requirements.component_contracts[0].sourceSubpath, undefined);

const envMountedPluginRoot = path.join(os.tmpdir(), 'homeboy-wordpress-component-checkout');
const envMountedPluginPath = path.join(envMountedPluginRoot, 'plugins', 'sample-plugin');
const envMountedPluginResult = buildWordPressFuzzRunnerResult({
	env: readWordPressFuzzRunnerEnv({
		HOMEBOY_FUZZ_WORKLOAD_PATH: '/unused/in-unit-test.json',
		HOMEBOY_FUZZ_WORKLOAD_ID: 'env-mounted-plugin',
		HOMEBOY_FUZZ_RUN_ID: 'env-mounted-plugin-run',
		HOMEBOY_RIG_COMPONENT_PATH__SAMPLE_PLUGIN: envMountedPluginPath,
		HOMEBOY_RIG_COMPONENT_CHECKOUT_ROOT__SAMPLE_PLUGIN: envMountedPluginRoot,
	}),
	workload: {
		schema: 'homeboy/fuzz-workload/v1',
		id: 'env-mounted-plugin',
		target: { type: 'wordpress-plugin', slug: 'sample-plugin', component: 'sample-plugin' },
		metadata: { fixture: { component: 'sample-plugin', activation: 'sample-plugin/sample-plugin.php' } },
		cases: [{ id: 'env-mounted-plugin:default', intent: { plugin: { activation: 'sample-plugin/sample-plugin.php' } } }],
	},
});
assert.deepEqual(envMountedPluginResult.wp_codebox_runtime_requirements.extra_plugins[0], {
	slug: 'sample-plugin',
	source: envMountedPluginPath,
	sourceRoot: envMountedPluginRoot,
	sourceSubpath: 'plugins/sample-plugin',
	path: envMountedPluginPath,
	pluginFile: 'sample-plugin/sample-plugin.php',
	loadAs: 'plugin',
	metadata: { component: 'sample-plugin', activation: 'fuzz-suite-setup-step' },
});
assert.equal(envMountedPluginResult.wp_codebox_task_request.executor.config.runtime_requirements.component_contracts[0].sourceRoot, envMountedPluginRoot);
assert.equal(envMountedPluginResult.wp_codebox_task_request.executor.config.runtime_requirements.component_contracts[0].sourceSubpath, 'plugins/sample-plugin');

const genericPrimitiveResult = buildWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		runId: 'generic-primitive-run',
		rigId: 'sample-rig',
		runtimeContext: {
			components: {
				'sample-plugin': { path: '/runner/components/sample-plugin' },
			},
		},
	},
	workload: {
		schema: 'homeboy/fuzz-workload/v1',
		id: 'generic-primitive-workload',
		label: 'Generic primitive workload',
		metadata: {
			generic_primitive: { command: 'wordpress.fuzz-admin-pages', status: 'preferred' },
		},
		target: { type: 'wordpress-plugin', slug: 'sample-plugin', component: 'sample-plugin' },
		workload: {
			runner: 'wp-codebox',
			type: 'php',
			path: '${package.root}/bench/admin-page-coverage.php',
			entry: 'admin-page-coverage',
		},
		cases: [{
			case_id: 'generic-primitive-workload:default',
			artifacts: [{ name: 'admin_page_coverage', path: 'admin-page-coverage/admin_page_coverage.json', required: true }],
			intent: {
				plugin: { activation: 'sample-plugin/sample-plugin.php' },
				execute: { path: '${package.root}/bench/admin-page-coverage.php', type: 'php', parameters: { safe_methods: 'GET', max_pages: '80' } },
				collect: [{ artifact: 'admin_page_coverage' }],
			},
		}],
	},
});
assert.deepEqual(genericPrimitiveResult.wp_codebox_input.cases[0].phases.action, [{ command: 'wordpress.fuzz-admin-pages', args: ['safe_methods=GET', 'max_pages=80'] }]);

let dispatchedRequest;
const dispatchPromise = runWordPressFuzzRunnerResult({
	env: {
		workloadPath: '/unused/in-unit-test.json',
		runId: 'dispatch-run',
	},
	workload,
	runRuntimeTask: async (request) => {
		dispatchedRequest = request;
		assert.equal(request.schema, 'homeboy/wp-codebox-fuzz-execution/v1');
		assert.equal(request.task_id, 'dispatch-run');
		assert.equal(request.ability, 'wp-codebox/run-fuzz-suite');
		assert.equal(request.input.schema, 'wp-codebox/fuzz-suite/v1');
		assert.equal(request.input.goal, 'Run WordPress fuzz suite generic-wordpress-workload and return the declared fuzz artifacts.');
		assert.equal(request.input.cases[0].target_id, 'rest-posts');
		assert.equal(request.executor, undefined);
		return {
			json: {
				schema: 'wp-codebox/fuzz-suite-result/v1',
				request_id: request.task_id,
				status: 'succeeded',
				summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
				cases: [{ id: 'get-posts', status: 'passed', success: true, diagnostics: [] }],
				coverage_summary: { surface_count: 1, exercised_count: 1 },
				artifactRefs: [
					{ path: 'dispatch/fuzz-report.json', kind: 'report', contentType: 'application/json' },
					{ path: 'dispatch/coverage.json', kind: 'coverage', contentType: 'application/json' },
				],
			},
		};
	},
}).then((dispatchedResult) => {
	assert(dispatchedRequest, 'runner should dispatch a Codebox fuzz suite request');
	assert.equal(dispatchedResult.status, 'succeeded');
	assert.equal(dispatchedResult.succeeded, true);
	assert.equal(dispatchedResult.wp_codebox_result.request_id, 'dispatch-run');
	assert.equal(dispatchedResult.wp_codebox_result.coverage_summary.surface_count, 1);
	assert.equal(dispatchedResult.wp_codebox_result.artifacts.some((artifact) => artifact.role === 'case_log'), true);
	assert.equal(dispatchedResult.wp_codebox_result.artifacts.some((artifact) => artifact.role === 'replay_data'), true);
	assert.equal(dispatchedResult.wp_codebox_result.artifacts.some((artifact) => artifact.role === 'coverage_summary'), true);
	assert.equal(dispatchedResult.homeboy_fuzz_campaign.metadata.artifact_refs[0].semantic_key, 'fuzz.report');
	assert.equal(dispatchedResult.homeboy_fuzz_campaign.metadata.artifact_refs[1].semantic_key, 'fuzz.coverage');
});

const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-fuzz-runner-'));
const workloadPath = path.join(tempDir, 'workload.json');
const runtimeRequirementWorkloadPath = path.join(tempDir, 'runtime-requirement-workload.json');
const workloadRoot = path.join(tempDir, 'workloads');
const resultsPath = path.join(tempDir, 'fuzz-results.json');
const runnerPath = path.join(__dirname, '..', 'scripts', 'fuzz', 'fuzz-runner.cjs');
const { discoverWpCodeboxBin, wpCodeboxCommand, wpCodeboxRuntimeEnv } = require(runnerPath);
fs.writeFileSync(workloadPath, `${JSON.stringify(workload)}\n`);
fs.writeFileSync(runtimeRequirementWorkloadPath, `${JSON.stringify({
	schema: 'homeboy/fuzz-workload/v1',
	id: 'runtime-requirement-workload',
	label: 'Runtime requirement workload',
	target: { type: 'wordpress-plugin', slug: 'sample-plugin', component: 'sample-plugin' },
	metadata: {
		fixture: { component: 'sample-plugin', activation: 'sample-plugin/sample-plugin.php' },
		homeboy_runtime_context: {
			schema: 'homeboy/fuzz-workload-runtime-context/v1',
			rig_id: 'sample-rig',
			components: {
				'sample-plugin': { path: '/runner/components/sample-plugin' },
			},
		},
	},
	cases: [{
		case_id: 'runtime-requirement-workload:default',
		intent: {
			type: 'wordpress-plugin-workload',
			plugin: { activation: 'sample-plugin/sample-plugin.php' },
			execute: { path: '/runner/workloads/sample.php', type: 'php' },
		},
	}],
})}\n`);
fs.mkdirSync(workloadRoot, { recursive: true });

assert.equal(fs.statSync(runnerPath).mode & 0o111, 0o111, 'fuzz runner script must be executable');
assert.equal(
	wpCodeboxRuntimeEnv({
		HOMEBOY_SETTINGS_WP_CODEBOX_CORE_MODULE: '/runner/wp-codebox/packages/runtime-core/dist/contracts.js',
	}).HOMEBOY_WP_CODEBOX_CORE_MODULE,
	'/runner/wp-codebox/packages/runtime-core/dist/contracts.js',
	'Lab-exported per-setting env should provide the WP Codebox core module path'
);

const codeboxInstallRoot = path.join(tempDir, 'wp-codebox-install');
const cachedCodeboxBin = path.join(codeboxInstallRoot, 'source', 'packages', 'cli', 'dist', 'index.js');
fs.mkdirSync(path.dirname(cachedCodeboxBin), { recursive: true });
fs.writeFileSync(cachedCodeboxBin, '#!/usr/bin/env node\n');
assert.equal(
	discoverWpCodeboxBin({ HOMEBOY_WP_CODEBOX_INSTALL_DIR: codeboxInstallRoot }),
	cachedCodeboxBin,
	'Fuzz runner should prefer the cached WP Codebox CLI over stale binaries on PATH'
);
assert.equal(
	wpCodeboxCommand({
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: codeboxInstallRoot,
		HOMEBOY_SETTINGS_WP_CODEBOX_BIN: '/settings/wp-codebox',
	}),
	'/settings/wp-codebox',
	'Explicit WP Codebox settings should override the Homeboy-managed cache'
);
assert.equal(
	wpCodeboxCommand({
		HOMEBOY_WP_CODEBOX_BIN: '/explicit/wp-codebox',
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: codeboxInstallRoot,
	}),
	'/explicit/wp-codebox',
	'Explicit WP Codebox env should override the Homeboy-managed cache'
);
assert.equal(
	wpCodeboxCommand({
		HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: '/settings-json/wp-codebox' }),
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: codeboxInstallRoot,
	}),
	'/settings-json/wp-codebox',
	'Explicit WP Codebox setting JSON should override the Homeboy-managed cache'
);

const cli = spawnSync(runnerPath, [], {
	encoding: 'utf8',
	env: {
		...process.env,
		HOMEBOY_WP_CODEBOX_FUZZ_DISPATCH: '0',
		HOMEBOY_FUZZ_WORKLOAD_PATH: workloadPath,
		HOMEBOY_FUZZ_WORKLOAD_ID: 'cli-workload',
		HOMEBOY_FUZZ_RUN_ID: 'cli-run',
		HOMEBOY_FUZZ_SEED: 'cli-seed',
		HOMEBOY_FUZZ_MAX_DURATION: '15',
		HOMEBOY_FUZZ_RESULTS_FILE: resultsPath,
	},
});

assert.equal(cli.status, 1, cli.stderr);
const cliResult = JSON.parse(cli.stdout);
assert.equal(cliResult.schema, WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA);
assert.equal(cliResult.status, 'unsupported');
assert.equal(cliResult.run_id, 'cli-run');
assert.equal(cliResult.succeeded, false);
assert.equal(cliResult.wp_codebox_input.metadata.limits.max_duration_seconds, 15);
const homeboyCampaign = JSON.parse(fs.readFileSync(resultsPath, 'utf8'));
assert.equal(homeboyCampaign.schema, HOMEBOY_FUZZ_CAMPAIGN_SCHEMA);
assert.equal(homeboyCampaign.version, 1);
assert.equal(homeboyCampaign.id, 'cli-run');
assert.equal(homeboyCampaign.metadata.diagnostics[0].code, 'wp_codebox_fuzz_suite_execution_unsupported');
assert.equal(homeboyCampaign.metadata.fuzz_result_envelope.schema, HOMEBOY_FUZZ_RESULT_ENVELOPE_SCHEMA);
assert.equal(homeboyCampaign.metadata.fuzz_result_envelope.campaign.workload_id, 'cli-workload');

const fakeCodeboxBin = path.join(tempDir, 'packages/cli/dist/fake-wp-codebox.js');
const emptyCodeboxInstallRoot = path.join(tempDir, 'empty-wp-codebox-install');
fs.mkdirSync(emptyCodeboxInstallRoot, { recursive: true });
fs.mkdirSync(path.dirname(fakeCodeboxBin), { recursive: true });
const dispatchResultsPath = path.join(tempDir, 'dispatch-results.json');
fs.writeFileSync(fakeCodeboxBin, `#!/usr/bin/env node
const fs = require('node:fs');
const subcommand = process.argv[2];
if (subcommand === 'fuzz' && process.argv[3] === 'readiness' && process.argv.includes('--format=json')) {
  process.stdout.write(JSON.stringify({
    schema: 'wp-codebox/fuzz-runner-readiness/v1',
    status: 'ready',
    mode: 'runtime-backed',
    entrypoint: 'run-fuzz-suite --runner-mode=runtime-backed',
    capabilities: {
      schema: 'wp-codebox/fuzz-runner-capabilities/v1',
      mode: 'runtime-backed',
      capabilities: ['target:runtime', 'runtime'],
      targetKinds: ['runtime'],
      operationKinds: ['read'],
      commands: ['run-fuzz-suite', 'wordpress.run-workload'],
      unsupportedRequiredCapabilities: []
    },
    unsupportedRequiredCapabilities: []
  }));
  process.exit(0);
}
if (subcommand === 'run-fuzz-suite' && process.argv.includes('--help')) {
  process.stdout.write('usage: wp-codebox run-fuzz-suite');
  process.exit(0);
}
if (subcommand === 'run-wordpress-workload' && process.argv.includes('--help')) {
  process.stderr.write('unknown command');
  process.exit(1);
}
const inputFileIndex = process.argv.indexOf('--input-file');
const request = inputFileIndex === -1 ? undefined : JSON.parse(fs.readFileSync(process.argv[inputFileIndex + 1], 'utf8'));
if (subcommand !== 'run-fuzz-suite' || inputFileIndex === -1 || process.argv.includes('--input=-') || !process.argv.includes('--format=json')) {
  process.stderr.write('expected public wp-codebox run-fuzz-suite command');
  process.exit(1);
}
if (request.schema !== 'wp-codebox/fuzz-suite/v1' || request.metadata?.homeboy_wp_codebox_fuzz_execution?.task_id !== 'dispatch-cli-run' || request.metadata?.homeboy_agent_task_request) {
  process.stderr.write('invalid public fuzz-suite input');
  process.exit(1);
}
process.stdout.write(JSON.stringify({
  schema: 'wp-codebox/fuzz-suite-result/v1',
  request_id: request.id,
  status: 'succeeded',
  summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
  cases: [{ id: 'get-posts', status: 'passed', success: true, diagnostics: [] }],
  artifactRefs: [
    { name: 'wp-codebox-fuzz-suite-result', path: 'fake/fuzz-report.json', kind: 'report', contentType: 'application/json' },
    { name: 'wordpress-fuzz-coverage', path: 'fake/coverage.json', kind: 'coverage', contentType: 'application/json' },
    { name: 'result-envelope', path: 'fake/envelope.json', contentType: 'application/json' },
    { name: 'case-log', path: 'fake/cases.jsonl', contentType: 'application/jsonl' },
    { name: 'replay-data', path: 'fake/replay.json', contentType: 'application/json' },
    { name: 'coverage-summary', path: 'fake/summary.json', contentType: 'application/json' },
    { name: 'wordpress-hotspots', path: 'fake/wordpress-hotspots.json', contentType: 'application/json', schema: 'wp-codebox/wordpress-hotspots/v1', semantic_key: 'fuzz.hotspot.codebox' }
  ],
  hotspot_summary: { schema: 'wp-codebox/wordpress-hotspots/v1', api: [{ route: '/wc/store/products', method: 'GET', metric: 'duration_ms', duration_ms: 42 }] },
  coverage_summary: { surface_count: 1, exercised_count: 1 }
}));
`);
fs.chmodSync(fakeCodeboxBin, 0o755);

const dispatchArtifactsDir = path.join(tempDir, 'dispatch-artifacts');

const dispatchCli = spawnSync(runnerPath, [], {
	encoding: 'utf8',
	env: {
		...process.env,
		HOMEBOY_WP_CODEBOX_BIN: fakeCodeboxBin,
		HOMEBOY_FUZZ_WORKLOAD_PATH: workloadPath,
		HOMEBOY_FUZZ_WORKLOAD_ID: 'dispatch-cli-workload',
		HOMEBOY_FUZZ_RUN_ID: 'dispatch-cli-run',
		HOMEBOY_FUZZ_RESULTS_FILE: dispatchResultsPath,
		HOMEBOY_FUZZ_ARTIFACTS_DIR: dispatchArtifactsDir,
	},
});

assert.equal(dispatchCli.status, 0, dispatchCli.stderr || dispatchCli.stdout);
const dispatchCliResult = JSON.parse(dispatchCli.stdout);
assert.equal(dispatchCliResult.succeeded, true, JSON.stringify(dispatchCliResult.wp_codebox_result));
assert.equal(dispatchCliResult.wp_codebox_result.request_id, 'dispatch-cli-run');
assert.equal(dispatchCliResult.homeboy_fuzz_campaign.metadata.artifact_refs[0].path, 'fake/fuzz-report.json');
const dispatchCampaign = JSON.parse(fs.readFileSync(dispatchResultsPath, 'utf8'));
assert.equal(dispatchCampaign.metadata.fuzz_result_envelope.gates.required_artifacts.every((artifact) => artifact.status === 'present'), true);
const dispatchHotspots = JSON.parse(fs.readFileSync(path.join(dispatchArtifactsDir, 'files', 'wordpress-hotspots.json'), 'utf8'));
assert.equal(dispatchHotspots.schema, 'homeboy/fuzz-hotspot-set/v1');
assert.equal(dispatchHotspots.items[0].value, 42);
assert.equal(fs.existsSync(path.join(dispatchArtifactsDir, 'files', 'wp-codebox-fuzz-suite-result.json')), true);
assert.equal(fs.existsSync(path.join(dispatchArtifactsDir, 'files', 'wordpress-fuzz-coverage.json')), true);
assert.equal(fs.existsSync(path.join(dispatchArtifactsDir, 'files', 'coverage-summary.json')), true);
assert.equal(fs.existsSync(path.join(dispatchArtifactsDir, 'files', 'case-log.jsonl')), true);
assert.equal(fs.existsSync(path.join(dispatchArtifactsDir, 'files', 'replay-data.json')), true);
assert.equal(fs.existsSync(path.join(dispatchArtifactsDir, 'dispatch-cli-workload', 'dispatch-cli-workload.json')), true);

const emptyHotspotArtifactsDir = path.join(tempDir, 'empty-hotspot-artifacts');
writeHomeboyFuzzArtifactFiles(emptyHotspotArtifactsDir, {
	wp_codebox_result: {
		wordpress_fuzz_result: {
			metadata: {
				artifacts: {
					wordpressHotspots: {
						schema: 'wp-codebox/wordpress-hotspots/v1',
						hotspots: [],
						summary: { total: 0 },
					},
				},
			},
		},
	},
});
const emptyHotspots = JSON.parse(fs.readFileSync(path.join(emptyHotspotArtifactsDir, 'files', 'wordpress-hotspots.json'), 'utf8'));
assert.equal(emptyHotspots.schema, 'wp-codebox/wordpress-hotspots/v1');
assert.deepEqual(emptyHotspots.hotspots, []);

const taskAdapterCodeboxBin = path.join(tempDir, 'packages/cli/dist/fake-task-adapter-wp-codebox.js');
fs.writeFileSync(taskAdapterCodeboxBin, `#!/usr/bin/env node
const fs = require('node:fs');
const command = process.argv[2];
if (command === 'run-fuzz-suite' && process.argv.includes('--help')) {
  process.stdout.write('usage: wp-codebox run-fuzz-suite');
  process.exit(0);
}
if (command === 'run-wordpress-workload' && process.argv.includes('--help')) {
  process.stdout.write('usage: wp-codebox run-wordpress-workload');
  process.exit(0);
}
const inputFile = process.argv[process.argv.indexOf('--input-file') + 1];
const request = JSON.parse(fs.readFileSync(inputFile, 'utf8'));
if (command !== 'run-fuzz-suite' || request.schema !== 'wp-codebox/fuzz-suite/v1' || request.metadata?.homeboy_wp_codebox_fuzz_execution?.task_id !== 'task-adapter-dispatch-cli-run') {
  process.stderr.write('invalid direct fuzz suite input');
  process.exit(1);
}
if (request.metadata?.homeboy_agent_task_request) {
  process.stderr.write('direct fuzz execution must not include agent-task metadata');
  process.exit(1);
}
if (request.metadata?.homeboy_wp_codebox_fuzz_execution?.schema !== 'homeboy/wp-codebox-fuzz-execution/v1') {
  process.stderr.write('missing direct fuzz execution envelope');
  process.exit(1);
}
if (request.metadata?.homeboy_wp_codebox_fuzz_execution?.input?.schema !== 'wp-codebox/fuzz-suite/v1') {
  process.stderr.write('missing delegated fuzz input');
  process.exit(1);
}
if (request.metadata?.homeboy_wp_codebox_fuzz_execution?.artifact_declarations?.[0]?.name !== 'wp-codebox-fuzz-suite-result' || !request.metadata?.homeboy_wp_codebox_fuzz_execution?.expected_artifacts?.includes('wordpress-fuzz-coverage')) {
  process.stderr.write('missing delegated artifact contract');
  process.exit(1);
}
if (request.metadata?.runtime_requirements?.extra_plugins?.[0]?.source !== '/runner/components/sample-plugin') {
  process.stderr.write('missing delegated runtime extra plugin');
  process.exit(1);
}
if (request.metadata?.runtime_requirements?.runtime_env?.WP_CODEBOX_FUZZ_WORKLOAD_ROOT !== '${workloadRoot}') {
  process.stderr.write('missing delegated fuzz workload root env');
  process.exit(1);
}
if (request.metadata?.homeboy_wp_codebox_fuzz_execution?.runtime_requirements?.runtime_env?.WP_CODEBOX_FUZZ_WORKLOAD_ROOT !== '${workloadRoot}') {
  process.stderr.write('missing direct execution runtime requirements');
  process.exit(1);
}
process.stdout.write(JSON.stringify({
  schema: 'wp-codebox/fuzz-suite-result/v1',
  request_id: request.id,
  status: 'succeeded',
  summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
  cases: [{ id: 'get-posts', status: 'passed', success: true, diagnostics: [] }],
  artifactRefs: [{ path: 'task-adapter/fuzz-report.json', kind: 'report', contentType: 'application/json' }],
  coverage_summary: { surface_count: 1, exercised_count: 1 }
}));
`);
fs.chmodSync(taskAdapterCodeboxBin, 0o755);

const taskAdapterDispatchCli = spawnSync(runnerPath, [], {
	encoding: 'utf8',
	env: {
		...process.env,
		HOMEBOY_WP_CODEBOX_FUZZ_DISPATCH: 'legacy-codebox-bin',
		HOMEBOY_WP_CODEBOX_BIN: taskAdapterCodeboxBin,
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: emptyCodeboxInstallRoot,
		HOMEBOY_WP_CODEBOX_PLUGIN_PATH: path.join(tempDir, 'packages/wordpress-plugin'),
		HOMEBOY_FUZZ_WORKLOAD_PATH: runtimeRequirementWorkloadPath,
		WP_CODEBOX_FUZZ_WORKLOAD_ROOT: workloadRoot,
		HOMEBOY_FUZZ_WORKLOAD_ID: 'task-adapter-dispatch-cli-workload',
		HOMEBOY_FUZZ_RUN_ID: 'task-adapter-dispatch-cli-run',
	},
});

assert.equal(taskAdapterDispatchCli.status, 0, taskAdapterDispatchCli.stderr);
const taskAdapterDispatchCliResult = JSON.parse(taskAdapterDispatchCli.stdout);
assert.equal(taskAdapterDispatchCliResult.succeeded, true);
assert.equal(taskAdapterDispatchCliResult.homeboy_fuzz_campaign.metadata.artifact_refs[0].path, 'task-adapter/fuzz-report.json');

dispatchPromise.then(() => {
	console.log('WordPress fuzz runner smoke passed.');
}).catch((error) => {
	console.error(error);
	process.exit(1);
});
