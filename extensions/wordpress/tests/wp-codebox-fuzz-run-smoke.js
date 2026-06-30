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
	WORDPRESS_FUZZ_OBSERVATION_SCHEMA,
	WP_CODEBOX_FUZZ_EXECUTION_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_SCHEMA,
	WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
	REQUIRED_WP_CODEBOX_FUZZ_CONTRACT_PATHS,
	buildWordPressFuzzCommandManifest,
	wpCodeboxFuzzSuiteAbility,
	wpCodeboxFuzzSuiteSchema,
	wpCodeboxWordPressWorkloadRunAbility,
	wpCodeboxWordPressWorkloadRunInput,
	wpCodeboxWordPressWorkloadRunSchema,
	normalizeWpCodeboxFuzzSuiteResult,
	detectWpCodeboxPublicFuzzCapabilities,
	preflightWpCodeboxFuzzCapabilityContract,
	publicFuzzCliRunnerModeForRequest,
	runWpCodeboxPublicFuzzOperation,
	runWpCodeboxFuzzSuite,
	validateWpCodeboxRuntimeRequirementMounts,
	wordpressFuzzPostprocessBinding,
	wpCodeboxFuzzExecutionRequest,
	wpCodeboxPublicCliInput,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzSuiteTaskRequest,
	wpCodeboxRuntimeContractManifest,
} = require('../lib/wp-codebox-fuzz-run');
const { buildWordPressFuzzRunnerResult } = require('../lib/wordpress-fuzz-runner');

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

const fixtureAndOptInInput = wpCodeboxFuzzSuiteInput({
	id: 'fixture-opt-in-suite',
	fixture_plan: {
		id: 'generic-fixtures',
		refs: [{ id: 'items', path: 'fixtures/items.json' }],
		data: { items: [{ id: 42 }] },
	},
	rest_mutation_opt_ins: {
		id: 'generic-rest-mutators',
		refs: ['artifact://rest-mutator-opt-ins'],
		entries: [
			{ id: 'create-item', route: '/example/v1/items', method: 'POST', fixture_ref: 'items' },
			{ id: 'patch-item', route: '/example/v1/items/42', method: 'PATCH', contract_ref: 'contract://patch-item' },
			{ id: 'delete-item', route: '/example/v1/items/42', method: 'DELETE', contract_ref: 'contract://delete-item' },
		],
	},
	cases: [{
		id: 'delete-item-case',
		input: {
			operation: { method: 'DELETE', route: '/example/v1/items/42' },
			rest_mutation_opt_in: { id: 'delete-item', route: '/example/v1/items/42', method: 'DELETE' },
		},
	}],
});
assert.equal(fixtureAndOptInInput.metadata.fixture_plan.schema, 'homeboy/wordpress-fuzz-fixture-plan/v1');
assert.equal(fixtureAndOptInInput.metadata.fixture_plan.refs[0].path, 'fixtures/items.json');
assert.equal(fixtureAndOptInInput.metadata.rest_mutation_opt_ins.schema, 'homeboy/wordpress-rest-mutation-opt-ins/v1');
assert.deepEqual(fixtureAndOptInInput.metadata.rest_mutation_opt_ins.entries.map((entry) => entry.method), ['POST', 'PATCH', 'DELETE']);
assert.equal(fixtureAndOptInInput.cases[0].input.rest_mutation_opt_in.method, 'DELETE');
assert(!JSON.stringify(fixtureAndOptInInput).includes('woocommerce'));

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
	commands: {
		wordpressRuntime: {
			runWorkload: 'run-wordpress-workload',
			runFuzzSuite: 'run-fuzz-suite',
		},
	},
	capabilities: {
		wordpressRuntime: {
			commands: ['run-fuzz-suite', 'run-wordpress-workload'],
			capabilities: ['rest', 'disposable-runtime', 'runtime-isolation', 'artifact-export'],
			runner_modes: { 'runtime-backed': true },
		},
	},
	readiness: {
		wordpressRuntime: {
			schema: 'wp-codebox/fuzz-runner-readiness/v1',
			status: 'ready',
			mode: 'runtime-backed',
			command_available: true,
		},
	},
};

assert.equal(wpCodeboxFuzzSuiteAbility({ runtimeContractManifest: manifest }), DEFAULT_FUZZ_SUITE_ABILITY);
assert.equal(wpCodeboxFuzzSuiteSchema({ runtimeContractManifest: manifest }), WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(wpCodeboxWordPressWorkloadRunAbility({ runtimeContractManifest: manifest }), DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY);
assert.equal(wpCodeboxWordPressWorkloadRunSchema({ runtimeContractManifest: manifest }), DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA);
assert.deepEqual(REQUIRED_WP_CODEBOX_FUZZ_CONTRACT_PATHS, [
	'abilities.wordpressRuntime.runFuzzSuite',
	'abilities.wordpressRuntime.runWorkload',
	'commands.wordpressRuntime.runFuzzSuite',
	'commands.wordpressRuntime.runWorkload',
	'schemas.wordpressRuntime.fuzzSuite',
	'schemas.wordpressRuntime.fuzzSuiteResult',
	'schemas.wordpressRuntime.workloadRun',
]);
assert.equal(wpCodeboxRuntimeContractManifest({ loadRuntimeContractSource: () => ({ manifest }) }), manifest);
const directContractSourceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-direct-contract-source-'));
try {
	const directCoreModule = path.join(directContractSourceRoot, 'contracts.cjs');
	fs.writeFileSync(directCoreModule, `module.exports.runtimeContractManifest = () => (${JSON.stringify(manifest)});\n`);
	assert.deepEqual(wpCodeboxRuntimeContractManifest({
		wpCodeboxCoreModule: directCoreModule,
		loadCanonicalRuntimeContractSourceSync: () => null,
	}), manifest);
} finally {
	fs.rmSync(directContractSourceRoot, { recursive: true, force: true });
}
const contractSourceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-contract-source-'));
try {
	const cacheRoot = path.join(contractSourceRoot, 'cache', 'wp-codebox');
	const cacheSourceRoot = path.join(cacheRoot, 'source');
	const staleSourceRoot = path.join(contractSourceRoot, 'wp-codebox@stale');
	const cacheCoreModule = path.join(cacheSourceRoot, 'packages', 'runtime-core', 'dist', 'contracts.js');
	fs.mkdirSync(path.join(cacheSourceRoot, 'packages', 'cli', 'dist'), { recursive: true });
	fs.mkdirSync(path.dirname(cacheCoreModule), { recursive: true });
	fs.mkdirSync(path.join(cacheSourceRoot, 'packages', 'runtime-playground', 'dist'), { recursive: true });
	fs.mkdirSync(path.join(staleSourceRoot, 'packages', 'runtime-core', 'dist'), { recursive: true });
	fs.writeFileSync(path.join(cacheSourceRoot, 'packages', 'cli', 'dist', 'index.js'), '#!/usr/bin/env node\n');
	fs.writeFileSync(cacheCoreModule, 'module.exports = {};\n');
	fs.writeFileSync(path.join(cacheSourceRoot, 'packages', 'runtime-playground', 'dist', 'index.js'), 'module.exports = {};\n');
	const staleCoreModule = path.join(staleSourceRoot, 'packages', 'runtime-core', 'dist', 'index.js');
	fs.writeFileSync(staleCoreModule, 'module.exports = {};\n');
	assert.equal(wpCodeboxRuntimeContractManifest({
		env: {
			HOMEBOY_WP_CODEBOX_BIN: path.join(staleSourceRoot, 'packages', 'cli', 'dist', 'index.js'),
			HOMEBOY_WP_CODEBOX_CORE_MODULE: staleCoreModule,
			HOMEBOY_WP_CODEBOX_INSTALL_DIR: cacheRoot,
		},
		loadRuntimeContractSource: (options) => {
			assert.equal(options.wpCodeboxCoreModule, cacheCoreModule);
			return { manifest };
		},
	}), manifest);
} finally {
	fs.rmSync(contractSourceRoot, { recursive: true, force: true });
}
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
			input: { type: 'artifact-root', path: '${artifacts.root}', max_bytes: 1024 },
			output: { artifact: 'coverage_gap_report', path: 'coverage/gaps.json', kind: 'json', schema: 'homeboy-rigs/wordpress-coverage-gap-report/v1', semantic_key: 'fuzz.coverage.gap_report' },
			parameters: { max_bytes: 1024 },
		},
	}],
});
assert.equal(artifactPostprocessWorkloadInput.steps[0].type, 'artifact-postprocess');
assert.equal(artifactPostprocessWorkloadInput.steps[0].helperPath, '${package.root}/tools/artifact-helper.mjs');
assert.equal(artifactPostprocessWorkloadInput.steps[0].action, 'coverage-gap-report');
assert.equal(artifactPostprocessWorkloadInput.steps[0].inputArtifactRoot, '${artifacts.root}');
assert.equal(artifactPostprocessWorkloadInput.steps[0].outputArtifactPath, 'coverage/gaps.json');
assert.equal(artifactPostprocessWorkloadInput.steps[0].maxInputBytes, 1024);
assert.equal(artifactPostprocessWorkloadInput.steps[0].expectedOutputSchema, 'homeboy-rigs/wordpress-coverage-gap-report/v1');
assert.equal(artifactPostprocessWorkloadInput.steps[0].artifactName, 'coverage_gap_report');
assert.equal(artifactPostprocessWorkloadInput.steps[0].artifactKind, 'json');
assert.equal(artifactPostprocessWorkloadInput.steps[0].semantic, 'fuzz.coverage.gap_report');
assert.deepEqual(artifactPostprocessWorkloadInput.steps[0].args, ['coverage-gap-report', '${inputArtifactRoot}', '${outputArtifactPath}', JSON.stringify({ max_bytes: 1024 })]);
assert.equal(artifactPostprocessWorkloadInput.steps[0].metadata.contract, 'homeboy/artifact-postprocess/v1');
const artifactPostprocessWorkloadInputAgain = wpCodeboxWordPressWorkloadRunInput(artifactPostprocessWorkloadInput);
assert.equal(artifactPostprocessWorkloadInputAgain.steps[0].helperPath, '${package.root}/tools/artifact-helper.mjs');
assert.equal(artifactPostprocessWorkloadInputAgain.steps[0].inputArtifactRoot, '${artifacts.root}');
assert.equal(artifactPostprocessWorkloadInputAgain.steps[0].outputArtifactPath, 'coverage/gaps.json');
const artifactPostprocessAbsoluteHelper = wpCodeboxWordPressWorkloadRunInput({
	packageRoot: '/runner/package',
	steps: [{ command: 'artifact-postprocess', args: { helper: '/runner/package/tools/artifact-helper.mjs', action: 'coverage-gap-report', input: { path: '${artifacts.root}' }, output: { path: 'coverage/gaps.json' } } }],
});
assert.equal(artifactPostprocessAbsoluteHelper.steps[0].helperPath, 'tools/artifact-helper.mjs');

const taskRequest = wpCodeboxFuzzSuiteTaskRequest({
	taskId: 'wp-codebox-fuzz-suite-smoke',
	input,
	provider: 'codex',
	runtimeId: 'wp-codebox',
});

assert.equal(taskRequest.executor.backend, 'wp-codebox');
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
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'wordpress-hotspots').schema, WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA);
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'wp-codebox-fuzz-suite-result').role, 'codebox_result');
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'case-log').role, 'case_log');
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'sandbox-isolation-proof').required, false);
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'mutation-isolation-artifact').required, false);
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'delete-boundary-artifact').required, false);
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'external-http-guardrail').semantic_key, 'fuzz.external_http.guardrail');
assert.equal(taskRequest.artifact_declarations.find((artifact) => artifact.name === 'runtime-access').semantic_key, 'fuzz.runtime.access');
assert.deepEqual(
	taskRequest.artifact_declarations.filter((artifact) => artifact.required === true).map((artifact) => artifact.name),
	taskRequest.expected_artifacts
);
assert(!JSON.stringify(taskRequest).includes('woocommerce'), 'fuzz suite helper must stay product-agnostic');
assert.equal(wpCodeboxFuzzSuiteTaskRequest({ taskId: 'suite-task' }).executor.config.runtime_task.input.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);

const destructiveTaskRequest = wpCodeboxFuzzSuiteTaskRequest({
	taskId: 'destructive-suite-task',
	input: {
		id: 'destructive-suite-task',
		metadata: { mode: 'aggressive' },
		cases: [{ id: 'delete-post-case', destructive: true }],
	},
});
assert.equal(destructiveTaskRequest.artifact_declarations.find((artifact) => artifact.name === 'sandbox-isolation-proof').required, true);
assert.equal(destructiveTaskRequest.artifact_declarations.find((artifact) => artifact.name === 'mutation-isolation-artifact').required, true);
assert.equal(destructiveTaskRequest.artifact_declarations.find((artifact) => artifact.name === 'delete-boundary-artifact').required, true);
assert(destructiveTaskRequest.expected_artifacts.includes('sandbox-isolation-proof'));
assert(destructiveTaskRequest.expected_artifacts.includes('mutation-isolation-artifact'));
assert(destructiveTaskRequest.expected_artifacts.includes('delete-boundary-artifact'));

const executionRequest = wpCodeboxFuzzExecutionRequest({ taskId: 'direct-suite-task', input, wpCodeboxBin: '/custom/wp-codebox', runtimeContractManifest: manifest });
assert.equal(executionRequest.schema, WP_CODEBOX_FUZZ_EXECUTION_SCHEMA);
assert.equal(executionRequest.task_id, 'direct-suite-task');
assert.equal(executionRequest.command, 'run-fuzz-suite');
assert.equal(executionRequest.input.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
assert.equal(executionRequest.metadata.executor, 'wp-codebox-direct-fuzz');
assert.equal(executionRequest.executor, undefined);

const runtimeRequirementRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-runtime-requirements-'));
const runtimePluginRoot = path.join(runtimeRequirementRoot, 'sample-checkout');
const runtimePluginPath = path.join(runtimePluginRoot, 'plugins', 'sample-plugin');
const runtimeWorkloadRoot = path.join(runtimeRequirementRoot, 'workloads');
fs.mkdirSync(runtimePluginPath, { recursive: true });
fs.mkdirSync(runtimeWorkloadRoot, { recursive: true });
const runtimeRequirementRequest = wpCodeboxFuzzExecutionRequest({
	taskId: 'runtime-requirement-suite-task',
	input,
	runtimeContractManifest: manifest,
	runtimeRequirements: {
		extra_plugins: [{ slug: 'sample-plugin', source: runtimePluginPath, sourceRoot: runtimePluginRoot, sourceSubpath: 'plugins/sample-plugin' }],
		component_contracts: [{ slug: 'sample-plugin', path: runtimePluginPath, sourceRoot: runtimePluginRoot, sourceSubpath: 'plugins/sample-plugin' }],
		runtime_mounts: [{ source: runtimeWorkloadRoot, target: runtimeWorkloadRoot, mode: 'readonly' }],
	},
});
const publicCliInput = wpCodeboxPublicCliInput(runtimeRequirementRequest);
assert.equal(publicCliInput.metadata.runtime_requirements.extra_plugins[0].sourceRoot, runtimePluginRoot);
assert.equal(publicCliInput.metadata.runtime_requirements.component_contracts[0].sourceSubpath, 'plugins/sample-plugin');
assert.deepEqual(publicCliInput.metadata.runtime_requirements.runtime_mounts, [{ source: runtimeWorkloadRoot, target: runtimeWorkloadRoot, mode: 'readonly' }]);

const monorepoPluginRuntimeRequirementRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-monorepo-plugin-'));
const monorepoPluginCheckoutRoot = path.join(monorepoPluginRuntimeRequirementRoot, 'checkout');
const monorepoPluginSource = path.join(monorepoPluginCheckoutRoot, 'plugins', 'woocommerce');
fs.mkdirSync(monorepoPluginSource, { recursive: true });
fs.writeFileSync(path.join(monorepoPluginSource, 'woocommerce.php'), '<?php\n/**\n * Plugin Name: WooCommerce\n */\n');
const monorepoPluginResult = buildWordPressFuzzRunnerResult({
	workload: {
		id: 'monorepo-plugin-file-path',
		target: { component: 'woocommerce', slug: 'woocommerce' },
		metadata: {
			homeboy_runtime_context: {
				schema: 'homeboy/rig-runtime-context/v1',
				rig_id: 'woocommerce-performance',
				components: {
					woocommerce: {
						path: monorepoPluginCheckoutRoot,
						extensions: {
							wordpress: {
								wp_codebox_source_subpath: 'plugins/woocommerce',
								wp_codebox_plugin_file: 'plugins/woocommerce/woocommerce.php',
							},
						},
					},
				},
			},
		},
		cases: [{ id: 'activate-woocommerce', intent: { plugin: { activation: 'woocommerce/woocommerce.php' } } }],
	},
	env: {
		workloadPath: path.join(monorepoPluginRuntimeRequirementRoot, 'workload.json'),
		runId: 'monorepo-plugin-file-path',
	},
});
assert.equal(monorepoPluginResult.wp_codebox_runtime_requirements.extra_plugins[0].pluginFile, 'plugins/woocommerce/woocommerce.php');
assert.equal(monorepoPluginResult.wp_codebox_runtime_requirements.component_contracts[0].pluginFile, 'plugins/woocommerce/woocommerce.php');
assert.throws(
	() => validateWpCodeboxRuntimeRequirementMounts({ runtime_mounts: [{ source: path.join(runtimeRequirementRoot, 'missing-workloads'), target: '/tmp/missing-workloads' }] }),
	/WP Codebox runtime requirement runtime_mounts\[0\] source does not exist/
);
assert.throws(
	() => wpCodeboxPublicCliInput(wpCodeboxFuzzExecutionRequest({
		taskId: 'missing-plugin-suite-task',
		input,
		runtimeContractManifest: manifest,
		runtimeRequirements: { extra_plugins: [{ slug: 'missing-plugin', source: path.join(runtimeRequirementRoot, 'missing-plugin') }] },
	})),
	/WP Codebox runtime requirement extra_plugins\[0\] source does not exist/
);

const preflightMissingCommand = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	runtimeContractManifest: manifest,
	publicCliCapabilities: { commands: { 'run-wordpress-workload': true } },
});
assert.equal(preflightMissingCommand.schema, WP_CODEBOX_FUZZ_PREFLIGHT_SCHEMA);
assert.equal(preflightMissingCommand.ok, false);
assert.deepEqual(preflightMissingCommand.missing_contracts.map((contract) => contract.command).filter(Boolean), ['run-fuzz-suite']);
assert.equal(preflightMissingCommand.diagnostics.some((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_public_cli_command'), true);
assert.equal(preflightMissingCommand.command_manifest.schema, 'homeboy/wordpress-fuzz-command-manifest/v1');
assert.deepEqual(preflightMissingCommand.command_manifest.case_intents['request-rest-route'].commands, ['run-wordpress-workload']);
assert.deepEqual(preflightMissingCommand.command_manifest.case_intents['request-rest-route'].runner_modes, ['runtime-backed']);

const commandManifest = buildWordPressFuzzCommandManifest();
assert.deepEqual(commandManifest.wp_codebox.public_commands, ['run-fuzz-suite', 'run-wordpress-workload']);
assert.equal(commandManifest.wp_codebox.abilities.runWorkload, DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY);
assert.deepEqual(commandManifest.wp_codebox.runner_modes, ['runtime-backed']);

const preflightMissingAbility = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	runtimeContractManifest: { schema: manifest.schema, abilities: { wordpressRuntime: { runFuzzSuite: DEFAULT_FUZZ_SUITE_ABILITY } } },
	publicCliCapabilities: { commands: { 'run-fuzz-suite': true, 'run-wordpress-workload': true } },
});
assert.equal(preflightMissingAbility.ok, false);
assert.equal(preflightMissingAbility.missing_contracts.some((contract) => contract.ability === DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY), true);
assert.equal(preflightMissingAbility.missing_contracts.some((contract) => contract.type === 'runtime_contract_manifest' && contract.missing_paths.includes('schemas.wordpressRuntime.workloadRun')), true);

const preflightMissingCanonicalContract = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	loadRuntimeContractSource: () => null,
	publicCliCapabilities: { commands: { 'run-fuzz-suite': true, 'run-wordpress-workload': true } },
});
assert.equal(preflightMissingCanonicalContract.ok, false);
assert.equal(preflightMissingCanonicalContract.missing_contracts.some((contract) => contract.type === 'runtime_contract_manifest'), true);
assert.equal(preflightMissingCanonicalContract.diagnostics.some((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_runtime_contract_manifest'), true);

const preflightPassed = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	runtimeContractManifest: manifest,
	publicCliCapabilities: { commands: { 'run-fuzz-suite': true, 'run-wordpress-workload': true } },
});
assert.equal(preflightPassed.ok, true);

const readinessContract = {
	schema: 'wp-codebox/fuzz-runner-readiness/v1',
	status: 'ready',
	mode: 'runtime-backed',
	entrypoint: 'run-fuzz-suite --runner-mode=runtime-backed',
	operationKinds: ['read', 'crud', 'mutation'],
	capabilities: {
		schema: 'wp-codebox/fuzz-runner-capabilities/v1',
		mode: 'runtime-backed',
		capabilities: ['target:runtime', 'runtime'],
		targetKinds: ['runtime'],
		operationKinds: ['read', 'crud', 'mutation'],
		runtimeActionTypes: ['crud_operation', 'rest_request', 'php', 'wp_cli'],
		commands: ['wordpress.crud-operation', 'wordpress.rest-request', 'wordpress.run-php', 'wordpress.wp-cli', 'wordpress.run-workload'],
		unsupportedRequiredCapabilities: [],
	},
	unsupportedRequiredCapabilities: [],
};
const readinessCapabilities = detectWpCodeboxPublicFuzzCapabilities({
	runtimeContractManifest: manifest,
	runPublicCli: () => {
		throw new Error('production dispatch must not probe fuzz readiness');
	},
});
assert.equal(readinessCapabilities.readiness.schema, 'wp-codebox/fuzz-runner-readiness/v1');
assert.equal(readinessCapabilities.commands['run-fuzz-suite'], true);
assert.equal(readinessCapabilities.commands['run-wordpress-workload'], true);
assert.deepEqual(readinessCapabilities.capabilities, ['artifact-export', 'disposable-runtime', 'rest', 'runtime-isolation']);
const preflightReadinessPassed = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	runtimeContractManifest: manifest,
	runPublicCli: () => {
		throw new Error('production dispatch must not probe fuzz readiness');
	},
});
assert.equal(preflightReadinessPassed.ok, true);
const preflightReadinessOnlyPassed = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	loadRuntimeContractSource: () => null,
	runPublicCli: () => {
		throw new Error('production dispatch must not probe fuzz readiness');
	},
});
assert.equal(preflightReadinessOnlyPassed.ok, false);
assert.equal(
	preflightReadinessOnlyPassed.missing_contracts.some((contract) => contract.type === 'runtime_contract_manifest' || contract.type === 'ability'),
	true,
);

const unsupportedReadiness = {
	...readinessContract,
	status: 'unsupported',
	unsupportedRequiredCapabilities: ['runtime-action:editor_insert_save'],
};
const preflightUnsupportedReadiness = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	runtimeContractManifest: manifest,
	publicCliReadiness: unsupportedReadiness,
});
assert.equal(preflightUnsupportedReadiness.ok, false);
assert.equal(preflightUnsupportedReadiness.missing_contracts.some((contract) => contract.type === 'public_cli_readiness'), true);
assert.equal(preflightUnsupportedReadiness.diagnostics.some((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_public_cli_readiness'), true);

const preflightMissingReadinessCommand = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	runtimeContractManifest: {
		...manifest,
		capabilities: undefined,
		readiness: undefined,
	},
	runPublicCli: () => {
		throw new Error('production dispatch must not probe fuzz readiness');
	},
});
assert.equal(preflightMissingReadinessCommand.ok, false);
assert.equal(preflightMissingReadinessCommand.missing_contracts.some((contract) => contract.type === 'explicit_public_descriptor'), true);
assert.equal(preflightMissingReadinessCommand.diagnostics.some((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_explicit_public_descriptor'), true);

const rollbackRestPlanRequest = wpCodeboxFuzzSuiteTaskRequest({
	taskId: 'rollback-rest-plan-task',
	input: {
		id: 'rollback-rest-plan-suite',
		workload: {
			plan: {
				targets: [{ cases: [{ intent: 'request-rest-route', required_capabilities: ['rest', 'checkpoint', 'rest-rollback'] }] }],
			},
		},
	},
});
const preflightMissingRuntimeCapability = preflightWpCodeboxFuzzCapabilityContract({
	request: rollbackRestPlanRequest,
	runtimeContractManifest: manifest,
	publicCliCapabilities: { commands: { 'run-fuzz-suite': true, 'run-wordpress-workload': true }, capabilities: ['rest', 'checkpoint'] },
});
assert.equal(preflightMissingRuntimeCapability.ok, false);
assert.equal(preflightMissingRuntimeCapability.missing_contracts.some((contract) => contract.type === 'runtime_capability' && contract.capability === 'rest-rollback'), true);
assert.equal(preflightMissingRuntimeCapability.diagnostics.some((diagnostic) => diagnostic.code === 'wp_codebox_fuzz_missing_runtime_capability'), true);

const mismatchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-fuzz-mismatch-'));
const mismatchCliRoot = path.join(mismatchRoot, 'wp-codebox@cli');
const mismatchCoreRoot = path.join(mismatchRoot, 'wp-codebox@core');
fs.mkdirSync(path.join(mismatchCliRoot, 'packages', 'cli', 'dist'), { recursive: true });
fs.mkdirSync(path.join(mismatchCliRoot, 'packages', 'runtime-playground', 'dist'), { recursive: true });
fs.mkdirSync(path.join(mismatchCoreRoot, 'packages', 'runtime-core', 'dist'), { recursive: true });
fs.writeFileSync(path.join(mismatchCliRoot, 'packages', 'cli', 'dist', 'index.js'), '#!/usr/bin/env node\n');
fs.writeFileSync(path.join(mismatchCliRoot, 'packages', 'runtime-playground', 'dist', 'index.js'), 'export const runtime = true;\n');
fs.writeFileSync(path.join(mismatchCoreRoot, 'packages', 'runtime-core', 'dist', 'index.js'), 'export const core = true;\n');
const preflightMismatch = preflightWpCodeboxFuzzCapabilityContract({
	request: taskRequest,
	runtimeContractManifest: manifest,
	publicCliCapabilities: { commands: { 'run-fuzz-suite': true, 'run-wordpress-workload': true } },
	env: { HOMEBOY_WP_CODEBOX_BIN: path.join(mismatchCliRoot, 'packages', 'cli', 'dist', 'index.js') },
	coreModule: path.join(mismatchCoreRoot, 'packages', 'runtime-core', 'dist', 'index.js'),
});
assert.equal(preflightMismatch.ok, false);
assert.equal(preflightMismatch.diagnostics[0].code, 'wp_codebox_identity_mismatch');
fs.rmSync(mismatchRoot, { recursive: true, force: true });

const tempPackageRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-fuzz-run-smoke-'));
const tempWorkloadDir = path.join(tempPackageRoot, 'bench');
fs.mkdirSync(tempWorkloadDir, { recursive: true });
const jsonWorkloadPath = path.join(tempWorkloadDir, 'json-workload-smoke.workload.json');
fs.writeFileSync(jsonWorkloadPath, `${JSON.stringify({
	id: 'json-workload-smoke',
	run: [{ command: 'wp-codebox/run-fuzz-suite', args: ['suite=${package.root}/manifests/codebox-fuzz-suite-smoke.json'] }],
	metadata: { fixture: 'json-workload-smoke', package_root: '${package.root}' },
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
	steps: [{ command: 'wp-codebox/run-fuzz-suite', args: [`suite=${tempPackageRoot}/manifests/codebox-fuzz-suite-smoke.json`] }],
	after: [],
	metadata: { fixture: 'json-workload-smoke', package_root: tempPackageRoot, source_path: jsonWorkloadPath, source_entry: 'wp-codebox/run-fuzz-suite' },
});
assert.deepEqual(jsonWorkloadInput.cases[0].phases.setup, [{ command: 'wordpress.plugin-state', args: ['action=activate', 'plugin=sample-plugin/sample-plugin.php'] }]);
assert.equal(JSON.stringify(jsonWorkloadInput).includes('wordpress.ensure-plugin-active'), false);
assert.equal(jsonWorkloadInput.cases[0].phases.action[0].command, 'wordpress.run-workload');
assert.deepEqual(JSON.parse(jsonWorkloadInput.cases[0].phases.action[0].args[0].replace(/^workload-json=/, '')).steps, [{ command: 'wp-codebox/run-fuzz-suite', args: [`suite=${tempPackageRoot}/manifests/codebox-fuzz-suite-smoke.json`] }]);
assert.deepEqual(jsonWorkloadInput.cases[0].phases.assert, [{ command: 'wordpress.collect-workload-result', args: ['artifact=json_fuzz_result'] }]);
assert.equal(jsonWorkloadInput.cases[0].artifacts[0].required, true);
assert.equal(jsonWorkloadInput.cases[0].artifacts[0].metadata.semantic_key, 'fuzz.suite_result');
assert.equal(jsonWorkloadInput.metadata.artifacts.expected[0].required, true);

const phpWorkloadPath = path.join(tempWorkloadDir, 'bench', 'rest-product-batch-import.php');
fs.mkdirSync(path.dirname(phpWorkloadPath), { recursive: true });
fs.writeFileSync(phpWorkloadPath, '<?php return function (): array { return array("status" => "passed"); };\n', 'utf8');
const phpWorkloadInput = wpCodeboxFuzzSuiteInput({
	id: 'php-workload-run',
	homeboyFuzzWorkload: {
		schema: 'homeboy/fuzz-workload/v1',
		id: 'php-workload',
		workload: { path: phpWorkloadPath, type: 'php', entry: 'rest-product-batch-import' },
		cases: [{ id: 'php-workload:default', intent: { execute: { path: phpWorkloadPath, type: 'php', entry: 'rest-product-batch-import' } } }],
	},
});
assert.equal(phpWorkloadInput.cases[0].input.schema, DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA);
assert.deepEqual(phpWorkloadInput.cases[0].input.steps, [
	{ command: 'wordpress.run-workload', args: [`path=${phpWorkloadPath}`, 'type=php'] },
]);

const wooDbApiWorkloadPath = path.join(tempWorkloadDir, 'rest-db-query-profile.workload.json');
fs.mkdirSync(path.join(tempWorkloadDir, 'bench'), { recursive: true });
fs.writeFileSync(path.join(tempWorkloadDir, 'bench', 'generated-rest-request-cases.php'), `<?php
return function (): array {
	return array('metadata' => array('generated_rest_request_cases_loaded' => true));
};
`, 'utf8');
fs.writeFileSync(wooDbApiWorkloadPath, `${JSON.stringify({
	id: 'rest-db-query-profile',
	run: [
		{ type: 'php', code: 'return array("loaded" => true);' },
		{ type: 'php', file: 'bench/generated-rest-request-cases.php' },
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
	{
		type: 'php',
		code: `$wp_codebox_embedded_callable = (function () {\nreturn function (): array {\n\treturn array('metadata' => array('generated_rest_request_cases_loaded' => true));\n};\n})(); return is_callable($wp_codebox_embedded_callable) ? $wp_codebox_embedded_callable() : $wp_codebox_embedded_callable;`,
		metadata: {
			source_file: path.join(tempWorkloadDir, 'bench', 'generated-rest-request-cases.php'),
			embedded_source_file: true,
		},
	},
	{ type: 'rest-db-query-profiler', 'metric-prefix': 'rest_db_query_profile', sampleLimit: 50 },
]);
assert.equal(wooDbApiInput.cases[0].phases.action[0].command, 'wordpress.run-workload');
assert.deepEqual(JSON.parse(wooDbApiInput.cases[0].phases.action[0].args[0].replace(/^workload-json=/, '')).steps, wooDbApiInput.cases[0].input.steps);
const wooDbApiSummary = normalizeWpCodeboxFuzzSuiteResult(wooDbApiFixture.result);
assert.equal(wooDbApiSummary.hotspot_summary.items[0].value, 12);
assert.equal(wooDbApiSummary.observation_set.observations[0].fingerprint, 'select-products');
assert.equal(wooDbApiSummary.observation_set.observations[1].metric, 'duration_ms');

const structuredResultSummary = normalizeWpCodeboxFuzzSuiteResult({
	schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
	status: 'passed',
	cases: [{ id: 'runtime-case', status: 'passed', success: true, metadata: { input: { id: 'runtime-workload' } } }],
	coverage_summary: { skipped_count: 0 },
}, { request: { artifact_declarations: DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS } });
assert.equal(structuredResultSummary.status, 'passed');
assert.deepEqual(structuredResultSummary.failures, []);
assert.equal(structuredResultSummary.artifacts.some((artifact) => artifact.name === 'wp-codebox-fuzz-suite-result'), true);
assert.equal(structuredResultSummary.artifacts.some((artifact) => artifact.name === 'case-log'), true);
assert.equal(structuredResultSummary.artifacts.some((artifact) => artifact.name === 'replay-data'), true);
assert.equal(structuredResultSummary.artifacts.some((artifact) => artifact.name === 'coverage-summary'), true);
assert.equal(structuredResultSummary.artifacts.some((artifact) => artifact.name === 'result-envelope' && artifact.role === 'result_envelope'), true);

const deleteBoundaryArtifactSummary = normalizeWpCodeboxFuzzSuiteResult({
	schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
	request_id: 'delete-boundary-artifacts',
	status: 'passed',
	cases: [{ id: 'delete-case', status: 'passed' }],
	delete_boundary_artifacts: [{
		name: 'delete-boundary-artifact',
		path: 'artifacts/delete-boundary.json',
		content_type: 'application/json',
		schema: 'wp-codebox/delete-boundary-artifact/v1',
		case_id: 'delete-case',
		operation_id: 'rest:posts:delete',
		status: 'passed',
	}],
});
const deleteBoundaryArtifact = deleteBoundaryArtifactSummary.artifacts.find((artifact) => artifact.role === 'delete_boundary_artifact');
assert.equal(deleteBoundaryArtifact.semantic_key, 'fuzz.delete.boundary');
assert.equal(deleteBoundaryArtifact.path, 'artifacts/delete-boundary.json');
assert.equal(deleteBoundaryArtifact.metadata.schema, 'wp-codebox/delete-boundary-artifact/v1');
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
assert.deepEqual(genericPrimitiveInput.cases[0].phases.setup, [{ command: 'wordpress.plugin-state', args: ['action=activate', 'plugin=sample-plugin/sample-plugin.php'] }]);
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
assert.deepEqual(planWorkloadInput.cases[0].phases.setup, [{ command: 'wordpress.plugin-state', args: ['action=activate', 'plugin=sample-plugin/sample-plugin.php'] }]);
assert.deepEqual(planWorkloadInput.cases[0].phases.action, [{
	command: 'wordpress.inventory-rest-routes',
	args: ['plugin=sample-plugin/sample-plugin.php', 'namespaces=sample/v1,sample/v2', 'artifact=route_inventory'],
}]);
assert.equal(JSON.stringify(planWorkloadInput).includes('/host-only/workload.php'), false);
assert.equal(planWorkloadInput.cases[0].metadata.source_plan_case, true);
assert.equal(planWorkloadInput.cases[0].metadata.target_id, 'sample-rest-routes');
assert.deepEqual(planWorkloadInput.cases[0].inputs.budget_keys, ['max_rest_p95_duration_ms']);

const genericPlanWorkloadInput = wpCodeboxFuzzSuiteInput({
	id: 'generic-plan-workload-run',
	homeboyFuzzWorkload: {
		schema: 'homeboy/fuzz-workload/v1',
		id: 'generic-plan-workload',
		plan: {
			schema: 'wordpress-fuzz-plan/v1',
			id: 'generic-plan',
			targets: [{
				id: 'hook:init',
				surface_id: 'hook:init',
				cases: [{ id: 'hook:init-generic-fuzz', intent: 'exercise-hook', operation: { hook: 'init' } }],
			}],
		},
	},
});
assert.equal(genericPlanWorkloadInput.cases[0].target.entrypoint, 'wordpress.run-fuzz-case');
assert.deepEqual(genericPlanWorkloadInput.cases[0].input, {
	case_id: 'hook:init-generic-fuzz',
	target_id: 'hook:init',
	surface_id: 'hook:init',
	intent: 'exercise-hook',
	operation: { hook: 'init' },
});
assert.equal(genericPlanWorkloadInput.cases[0].phases.action[0].command, 'wordpress.run-fuzz-case');
assert.deepEqual(genericPlanWorkloadInput.cases[0].phases.action[0].args, [
	'case_id=hook:init-generic-fuzz',
	'target_id=hook:init',
	'surface_id=hook:init',
	'intent=exercise-hook',
	'operation={"hook":"init"}',
]);

const descriptorPlanWorkloadInput = wpCodeboxFuzzSuiteInput({
	id: 'descriptor-plan-workload-run',
	homeboyFuzzWorkload: {
		schema: 'homeboy/fuzz-workload/v1',
		id: 'descriptor-plan-workload',
		plan: {
			schema: 'wordpress-fuzz-plan/v1',
			id: 'descriptor-plan',
			targets: [{
				id: 'rest:posts',
				surface_id: 'rest:posts',
				cases: [{
					id: 'rest:posts-get',
					intent: 'request-rest-route',
					operation: { method: 'GET', route: '/wp/v2/posts' },
					runtime_operation: { schema: 'homeboy/wordpress-fuzz-runtime-workload-operation/v1', command: 'wordpress.rest-request', family: 'rest', status: 'ready' },
				}],
			}],
		},
	},
});
assert.equal(descriptorPlanWorkloadInput.cases[0].target.kind, 'runtime-action');
assert.equal(descriptorPlanWorkloadInput.cases[0].target.entrypoint, 'rest_request');
assert.equal(descriptorPlanWorkloadInput.cases[0].phases, undefined);
assert.deepEqual(descriptorPlanWorkloadInput.cases[0].input, { type: 'rest_request', method: 'GET', path: '/wp/v2/posts' });

const genericPlanTaskRequest = wpCodeboxFuzzSuiteTaskRequest({ taskId: 'generic-plan-task', input: genericPlanWorkloadInput });
const genericPlanPreflight = preflightWpCodeboxFuzzCapabilityContract({
	request: genericPlanTaskRequest,
	runtimeContractManifest: manifest,
	publicCliCapabilities: { commands: { 'run-fuzz-suite': true } },
});
assert.equal(genericPlanPreflight.ok, true);
assert.deepEqual(genericPlanPreflight.required.commands, ['run-fuzz-suite']);
assert.equal(publicFuzzCliRunnerModeForRequest(genericPlanTaskRequest), 'runtime-backed');

let invoked = false;
runWpCodeboxFuzzSuite({
	taskId: 'wp-codebox-fuzz-suite-delegation-smoke',
	input,
	runFuzzSuite: async (request) => {
		invoked = true;
		assert.equal(request.schema, WP_CODEBOX_FUZZ_EXECUTION_SCHEMA);
		assert.equal(request.input.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
		assert.equal(request.executor, undefined);
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
					hotspot_summary: { name: 'wordpress-hotspots', path: 'reports/hotspots.json', content_type: 'application/json', schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA, payload: { schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA, db: [{ table: 'wp_posts', operation: 'SELECT', metric: 'query_count', count: 4 }], api: [{ route: '/wp-json/wp/v2/posts', method: 'GET', metric: 'duration_ms', duration_ms: 99 }] } },
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
	assert.equal(summary.hotspot_summary.items.some((item) => item.dimension === 'api' && item.value === 99), true);
	assert.equal(summary.hotspot_summary.items.some((item) => item.dimension === 'database' && item.metadata.surface_key === 'wp_posts'), true);
	assert.equal(summary.observation_set.schema, 'homeboy/fuzz-observation-set/v1');
	assert.equal(summary.observation_set.observations[0].family, 'query');
	assert.equal(summary.observation_set.observations[1].metric, 'duration_ms');
	assert.equal(summary.observation_set.observations.some((observation) => observation.case_id === 'case-000' && observation.metric === 'query_count'), true);
	assert.equal(summary.runtime_task_result.observation_set.observations[0].fingerprint, 'select-posts');
	assert.equal(summary.derived_artifacts.artifacts.some((artifact) => artifact.role === 'hotspot_summary'), true);
	assert.deepEqual(summary.artifacts.map((artifact) => artifact.role), ['fuzz_report', 'coverage', 'normalized_fuzz_result', 'coverage_gap_report', 'hotspot_summary', 'fuzz_case', 'failing_case', 'case_artifact', 'repro_case', 'repro_case', 'result_envelope']);
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
	assert.equal(summary.artifacts.some((artifact) => artifact.role === 'result_envelope'), true);
	assert.equal(summary.artifacts.some((artifact) => artifact.name === 'placeholder-only'), false);
	assert.equal(summary.observation.schema, WORDPRESS_FUZZ_OBSERVATION_SCHEMA);
	assert.equal(summary.observation.status, 'succeeded');
	assert.equal(summary.observation.succeeded, true);
	assert.equal(summary.observation.summary.total, 2);
	assert.equal(summary.observation.summary.coverage.surface_count, 3);
	assert.equal(summary.observation.metrics.coverage.exercised_count, 1);
	assert.equal(summary.observation.metrics.db_query.query_count, 1);
	assert.equal(summary.observation.metrics.hotspots.count, 2);
	assert.equal(summary.observation.artifacts.find((artifact) => artifact.role === 'normalized_fuzz_result').semantic_key, 'fuzz.result.normalized');
	assert.equal(summary.observation.normalized_result.schema, 'wordpress-fuzz-result/v1');

	const wooFinalArtifactShape = {
		schema: 'homeboy/fuzz-campaign/v1',
		version: 1,
		id: 'woo-db-api-rest-query-profile-20260625-16',
		metadata: {
			status: 'passed',
			success: true,
			artifact_refs: [{
				role: 'fuzz_report',
				semantic_key: 'fuzz.result.normalized',
				name: 'wp-codebox-fuzz-suite-result',
				content: {
					schema: 'wp-codebox/fuzz-suite-result/v1',
					suite: { id: 'woo-db-api-rest-query-profile-20260625-16' },
					status: 'passed',
					success: true,
					summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
					coverageSummary: { discovered: 1, generated: 1, executed: 1, skipped: 0, untested: 0, skippedReasons: [] },
					cases: [{
						id: 'rest-db-query-profile:default',
						status: 'passed',
						success: true,
						target: { kind: 'runtime', id: 'wordpress.run-workload', entrypoint: 'wordpress.run-workload' },
						diagnostics: [],
						metadata: {
							input: { schema: 'wp-codebox/wordpress-workload-run/v1', id: 'rest-db-query-profile' },
							description: 'REST DB query profile coverage',
							adapter: 'wp-codebox',
						},
					}],
				},
			}],
		},
	};
	const wooObservation = normalizeWpCodeboxFuzzSuiteResult(wooFinalArtifactShape.metadata.artifact_refs[0].content).observation;
	assert.equal(wooObservation.schema, WORDPRESS_FUZZ_OBSERVATION_SCHEMA);
	assert.equal(wooObservation.id, 'woo-db-api-rest-query-profile-20260625-16');
	assert.equal(wooObservation.status, 'passed');
	assert.equal(wooObservation.summary.total, 1);
	assert.equal(wooObservation.summary.coverage.discovered_count, 1);
	assert.equal(wooObservation.summary.coverage.generated_count, 1);
	assert.equal(wooObservation.summary.coverage.exercised_count, 1);
	assert.equal(wooObservation.summary.coverage.untested_count, 0);
	assert.equal(wooObservation.source.result_schema, 'wp-codebox/fuzz-suite-result/v1');
	assert.equal(wooObservation.normalized_result.cases[0].id, 'rest-db-query-profile:default');
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
		['result_envelope', 'fuzz.result.envelope'],
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
	const requiredOutputRequest = wpCodeboxFuzzSuiteTaskRequest({
		taskId: 'required-output-suite-task',
		artifactDeclarations: [],
		input: wpCodeboxFuzzSuiteInput({
			id: 'required-output-suite',
			metadata: {
				output_requirements: {
					required_normalized_metric_paths: ['cases.*.performance_metrics.query_count'],
					required_artifact_keys: ['fuzz.result.normalized'],
					required_evidence_statuses: [
						{ path: 'normalizedResult.cases.*.performance_metric_reasons.query_count.status', status: 'observed' },
					],
				},
			},
		}),
	});
	const requiredOutputObserved = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			status: 'passed',
			summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
			wordpress_fuzz_result: {
				schema: 'wordpress-fuzz-result/v1',
				status: 'passed',
				cases: [{
					id: 'required-output-case',
					status: 'passed',
					performance_metrics: { query_count: 0 },
					performance_metric_reasons: { query_count: { status: 'observed' } },
				}],
			},
			artifactRefs: [{ name: 'wordpress-fuzz-result', role: 'normalized_fuzz_result', semantic_key: 'fuzz.result.normalized', path: 'wordpress-fuzz-result.json' }],
		},
	}, { request: requiredOutputRequest });
	assert.equal(requiredOutputObserved.succeeded, true);
	assert.equal(requiredOutputObserved.failures.length, 0);
	const hotspotArtifactRequest = wpCodeboxFuzzSuiteTaskRequest({
		taskId: 'required-hotspot-artifact-suite-task',
		artifactDeclarations: [],
		input: wpCodeboxFuzzSuiteInput({
			id: 'required-hotspot-artifact-suite',
			metadata: {
				output_requirements: {
					required_artifact_keys: ['fuzz.hotspot.summary'],
					required_artifact_schemas: [WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA],
				},
			},
		}),
	});
	const hotspotArtifactObserved = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			status: 'passed',
			summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
			cases: [{ id: 'required-hotspot-case', status: 'passed' }],
			artifacts: {
				hotspot_summary: {
					name: 'wordpress-hotspots',
					path: 'reports/wordpress-hotspots.json',
					schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
					payload: {
						schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
						db: [{ table: 'wp_posts', operation: 'SELECT', metric: 'query_count', count: 7 }],
						api: [{ route: '/wp-json/wp/v2/posts', method: 'GET', metric: 'duration_ms', duration_ms: 33 }],
					},
				},
			},
		},
	}, { request: hotspotArtifactRequest });
	assert.equal(hotspotArtifactObserved.succeeded, true);
	assert.equal(hotspotArtifactObserved.hotspot_summary.items.some((item) => item.dimension === 'database' && item.value === 7), true);
	assert.equal(hotspotArtifactObserved.hotspot_summary.items.some((item) => item.dimension === 'api' && item.value === 33), true);
	const hotspotArtifactMissing = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			status: 'passed',
			summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
			cases: [{ id: 'missing-hotspot-case', status: 'passed' }],
			artifacts: { coverage: { path: 'coverage.json', semantic_key: 'fuzz.coverage' } },
		},
	}, { request: hotspotArtifactRequest });
	assert.equal(hotspotArtifactMissing.succeeded, false);
	assert(hotspotArtifactMissing.failures.some((failure) => failure.code === 'wp_codebox_fuzz_required_output_artifacts_missing'));
	assert(hotspotArtifactMissing.failures.some((failure) => failure.code === 'wp_codebox_fuzz_required_output_artifact_schemas_missing'));
	assert(hotspotArtifactMissing.failures.some((failure) => failure.code === 'wp_codebox_fuzz_required_hotspot_artifact_missing'));
	const productionPostprocessRequest = wpCodeboxFuzzSuiteTaskRequest({
		taskId: 'production-postprocess-suite-task',
		artifactDeclarations: [],
		input: wpCodeboxFuzzSuiteInput({
			id: 'production-postprocess-suite',
			metadata: {
				production_campaign: true,
				postprocess_binding: wordpressFuzzPostprocessBinding(),
			},
		}),
	});
	const productionPostprocessObserved = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			status: 'passed',
			summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
			wordpress_fuzz_result: {
				schema: 'wordpress-fuzz-result/v1',
				status: 'passed',
				cases: [{ id: 'production-postprocess-case', status: 'passed' }],
			},
			artifacts: {
				coverage: { path: 'coverage.json', semantic_key: 'fuzz.coverage' },
				gap_report: {
					path: 'gap-report.json',
					semantic_key: 'fuzz.coverage.gap_report',
					payload: { schema: 'homeboy/wordpress-fuzz-coverage-gap-report/v1', gaps: [] },
				},
				hotspot_summary: {
					path: 'wordpress-hotspots.json',
					schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
					payload: { schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA, db: [{ table: 'wp_posts', operation: 'SELECT', metric: 'query_count', count: 1 }] },
				},
			},
		},
	}, { request: productionPostprocessRequest });
	assert.equal(productionPostprocessObserved.succeeded, true);
	assert.equal(productionPostprocessObserved.failures.some((failure) => failure.code === 'wp_codebox_fuzz_required_postprocess_outputs_missing'), false);
	const productionPostprocessMissing = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			status: 'passed',
			summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
			cases: [{ id: 'production-postprocess-case', status: 'passed' }],
			artifacts: { coverage: { path: 'coverage.json', semantic_key: 'fuzz.coverage' } },
		},
	}, { request: productionPostprocessRequest });
	assert.equal(productionPostprocessMissing.succeeded, false);
	assert.deepEqual(
		productionPostprocessMissing.failures.find((failure) => failure.code === 'wp_codebox_fuzz_required_postprocess_outputs_missing').missing_outputs,
		['fuzz.hotspot.summary', 'fuzz.coverage.gap_report', 'fuzz.hotspot.codebox']
	);
	const requiredOutputMissing = normalizeWpCodeboxFuzzSuiteResult({
		json: {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			status: 'passed',
			summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
			wordpress_fuzz_result: {
				schema: 'wordpress-fuzz-result/v1',
				status: 'passed',
				cases: [{ id: 'required-output-case', status: 'passed', performance_metric_reasons: { query_count: { status: 'missing' } } }],
			},
		},
	}, { request: requiredOutputRequest });
	assert.equal(requiredOutputMissing.succeeded, false);
	assert(requiredOutputMissing.failures.some((failure) => failure.code === 'wp_codebox_fuzz_required_normalized_metrics_missing'));
	assert(requiredOutputMissing.failures.some((failure) => failure.code === 'wp_codebox_fuzz_required_output_artifacts_missing'));
	assert(requiredOutputMissing.failures.some((failure) => failure.code === 'wp_codebox_fuzz_required_evidence_status_missing'));
	assert.equal(normalizeWpCodeboxFuzzSuiteResult({ status: 'passed' }).result_schema, WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA);
	assert.equal(normalizeWpCodeboxFuzzSuiteResult({ status: 'passed' }).schema, WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA);
	return runWpCodeboxFuzzSuite({ taskId: 'suite-run', runFuzzSuite: async () => ({ status: 'passed' }) });
}).then((summary) => {
	assert.equal(summary.delegated_schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
	return runWpCodeboxFuzzSuite({
		taskId: 'public-cli-suite-run',
		input,
		runtimeContractManifest: manifest,
		wpCodeboxBin: '/custom/direct-wp-codebox',
		runtimeRequirements: {
			extra_plugins: [{ slug: 'sample-plugin', source: runtimePluginPath, loadAs: 'plugin' }],
			runtime_env: { WP_CODEBOX_FUZZ_WORKLOAD_ROOT: runtimeWorkloadRoot },
		},
		runPublicCli: ({ command, args, stdin }) => {
			assert.equal(command, '/custom/direct-wp-codebox');
			if (args.join(' ') === 'fuzz readiness --format=json') {
				return { status: 0, stdout: JSON.stringify(readinessContract) };
			}
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
			const publicCliInput = JSON.parse(fs.readFileSync(args[2], 'utf8'));
			assert.equal(publicCliInput.schema, WP_CODEBOX_FUZZ_SUITE_SCHEMA);
			assert.equal(publicCliInput.metadata.runtime_requirements.extra_plugins[0].source, runtimePluginPath);
			assert.equal(publicCliInput.metadata.runtime_requirements.runtime_env.WP_CODEBOX_FUZZ_WORKLOAD_ROOT, runtimeWorkloadRoot);
			assert.equal(publicCliInput.metadata.homeboy_wp_codebox_fuzz_execution.schema, WP_CODEBOX_FUZZ_EXECUTION_SCHEMA);
			assert.equal(publicCliInput.metadata.homeboy_wp_codebox_fuzz_execution.expected_artifacts.includes('case-log'), true);
			assert.equal(publicCliInput.metadata.homeboy_agent_task_request, undefined);
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
	const stagedHelperDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-staged-helper-'));
	fs.mkdirSync(path.join(stagedHelperDir, 'bench'));
	fs.mkdirSync(path.join(stagedHelperDir, 'tools'));
	const stagedWorkloadPath = path.join(stagedHelperDir, 'bench', 'coverage-gap-report.workload.json');
	const stagedHelperPath = path.join(stagedHelperDir, 'tools', 'artifact-helper.mjs');
	const stagedArtifactRoot = path.join(stagedHelperDir, 'artifacts');
	fs.mkdirSync(stagedArtifactRoot);
	fs.writeFileSync(stagedHelperPath, 'export {};\n', 'utf8');
	const stagedHelperInput = wpCodeboxFuzzSuiteInput({
		id: 'staged-helper-suite',
		cases: [{
			id: 'staged-helper-case',
			target: { kind: 'runtime', id: 'wordpress.run-workload', entrypoint: 'wordpress.run-workload' },
			input: wpCodeboxWordPressWorkloadRunInput({
				id: 'staged-helper-workload',
				packageRoot: stagedHelperDir,
				steps: [{ command: 'artifact-postprocess', args: { helper: stagedHelperPath, action: 'coverage-gap-report', input: { path: '${artifacts.root}' }, output: { path: 'coverage/gaps.json' } } }],
				metadata: { source_path: stagedWorkloadPath },
			}),
		}],
	});
	return runWpCodeboxFuzzSuite({
		taskId: 'public-cli-staged-helper-run',
		input: stagedHelperInput,
		runtimeContractManifest: manifest,
		wpCodeboxBin: '/custom/direct-wp-codebox',
		runtimeRequirements: { extra_plugins: [{ slug: 'sample-plugin', source: stagedHelperDir, loadAs: 'plugin' }] },
		env: { resultsFile: path.join(stagedArtifactRoot, 'fuzz-results.json') },
		runPublicCli: ({ args }) => {
			if (args.join(' ') === 'fuzz readiness --format=json') return { status: 0, stdout: JSON.stringify(readinessContract) };
			if (args.includes('--help')) return { status: 0, stdout: 'usage' };
			assert.deepEqual([args[0], args[1], args[2], args[4]], ['run-fuzz-suite', '--runner-mode=runtime-backed', '--input-file', '--json']);
			const publicCliInput = JSON.parse(fs.readFileSync(args[3], 'utf8'));
			const workload = publicCliInput.cases[0].input;
			const stagedFiles = publicCliInput.cases[0].input.staged_files;
			assert.equal(workload.steps[0].helperPath, 'tools/artifact-helper.mjs');
			assert.equal(workload.steps[0].inputArtifactRoot, '/tmp/wp-codebox-artifacts');
			assert.deepEqual(workload.mounts, [{ source: stagedArtifactRoot, target: '/tmp/wp-codebox-artifacts', mode: 'readwrite' }]);
			assert.deepEqual(stagedFiles, [{ source: stagedHelperPath, target: '/wordpress/wp-content/plugins/sample-plugin/tools/artifact-helper.mjs' }]);
			return { status: 0, stdout: JSON.stringify({ schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA, status: 'passed', summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 } }) };
		},
	}).finally(() => fs.rmSync(stagedHelperDir, { recursive: true, force: true }));
}).then((summary) => {
	assert.equal(summary.succeeded, true);
	const largeCliDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-large-codebox-cli-'));
	const largeCli = path.join(largeCliDir, 'wp-codebox-large-output.cjs');
	fs.writeFileSync(largeCli, `
const args = process.argv.slice(2);
if (args.join(' ') === 'fuzz readiness --format=json') {
  process.stdout.write(${JSON.stringify(JSON.stringify(readinessContract))});
  process.exit(0);
}
if (args.includes('--help')) {
  process.stdout.write('usage');
  process.exit(0);
}
process.stdout.write(JSON.stringify({
  schema: ${JSON.stringify(WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA)},
  status: 'passed',
  summary: { total: 1, passed: 1, failed: 0, error: 0, skipped: 0 },
  cases: [{ id: 'large-output-case', status: 'passed' }],
  metadata: { large: 'x'.repeat(11 * 1024 * 1024) }
}));
`, 'utf8');
	return runWpCodeboxFuzzSuite({
		taskId: 'public-cli-large-output-run',
		input,
		runtimeContractManifest: manifest,
		wpCodeboxBin: largeCli,
	}).finally(() => fs.rmSync(largeCliDir, { recursive: true, force: true }));
}).then((summary) => {
	assert.equal(summary.succeeded, true);
	assert.equal(summary.result_schema, WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA);
	assert.deepEqual(detectWpCodeboxPublicFuzzCapabilities({ publicCliCapabilities: { commands: { 'run-wordpress-workload': true } } }).commands, {
		'run-fuzz-suite': false,
		'run-wordpress-workload': true,
	});
	return runWpCodeboxFuzzSuite({
		taskId: 'public-cli-unsupported-run',
		input,
		runtimeContractManifest: { ...manifest, capabilities: undefined, readiness: undefined },
		runPublicCli: () => {
			throw new Error('production dispatch must not probe or execute without explicit descriptors');
		},
	});
}).then((summary) => {
	assert.equal(summary.succeeded, false);
	assert.equal(summary.failures[0].code, 'wp_codebox_fuzz_missing_explicit_public_descriptor');
	assert.equal(summary.failures.some((failure) => failure.code === 'wp_codebox_fuzz_required_artifacts_missing'), false);

	console.log('wp-codebox fuzz-run smoke passed');
}).catch((error) => {
	console.error(error);
	process.exit(1);
});
