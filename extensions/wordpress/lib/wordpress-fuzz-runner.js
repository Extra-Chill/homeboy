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
const { normalizeWordPressFuzzRuntimeCapabilities } = require('./wordpress-fuzz-runtime-capabilities');
const {
	normalizeWpCodeboxFuzzSuiteResult,
	runWpCodeboxFuzzSuite,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzRuntimeTaskRequest,
	wpCodeboxFuzzSuiteTaskRequest,
} = require('./wp-codebox-fuzz-run');
const { aggregateWordPressFuzzCoverage } = require('./wordpress-fuzz-coverage-aggregate');
const {
	requiredWpCodeboxContractsForFuzzCase,
	requiredWpCodeboxContractsForFuzzPlan,
} = require('./wordpress-fuzz-command-manifest');
const { createCodeboxClient } = require('./codebox-client');

const WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA = 'homeboy/wordpress-fuzz-runner-result/v1';
const HOMEBOY_FUZZ_CAMPAIGN_SCHEMA = 'homeboy/fuzz-campaign/v1';
const HOMEBOY_FUZZ_RESULT_ENVELOPE_SCHEMA = 'homeboy/fuzz-result-envelope/v1';
const HOMEBOY_FUZZ_CONTRACT_VERSION = 1;

function readWordPressFuzzRunnerEnv(env = process.env) {
	return stripUndefined({
		workloadPath: env.HOMEBOY_FUZZ_WORKLOAD_PATH,
		workloadId: env.HOMEBOY_FUZZ_WORKLOAD_ID,
		runId: env.HOMEBOY_FUZZ_RUN_ID,
		seed: env.HOMEBOY_FUZZ_SEED,
		maxDuration: env.HOMEBOY_FUZZ_MAX_DURATION,
		executionRequest: readFuzzExecutionRequest(env.HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE),
		resultsFile: env.HOMEBOY_FUZZ_RESULTS_FILE,
		artifactRoot: env.HOMEBOY_FUZZ_ARTIFACTS_DIR || env.HOMEBOY_ARTIFACT_ROOT || env.HOMEBOY_ARTIFACT_DIR || env.HOMEBOY_ARTIFACTS_DIR || env.HOMEBOY_RUN_ARTIFACT_ROOT || env.HOMEBOY_RUN_ARTIFACT_DIR,
		wpCodeboxFuzzWorkloadRoot: env.WP_CODEBOX_FUZZ_WORKLOAD_ROOT,
		wpCodeboxBin: env.HOMEBOY_WP_CODEBOX_BIN || env.WP_CODEBOX_BIN || env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN,
		wpCliBin: env.HOMEBOY_WP_CLI_BIN || env.WP_CLI_BIN,
		componentPathOverrides: componentPathOverridesFromEnv(env, 'HOMEBOY_RIG_COMPONENT_PATH__'),
		componentCheckoutRootOverrides: componentPathOverridesFromEnv(env, 'HOMEBOY_RIG_COMPONENT_CHECKOUT_ROOT__'),
	});
}

function buildWordPressFuzzRunnerResult(options = {}) {
	const context = buildWordPressFuzzRunnerContext(options);
	const codeboxResult = normalizeCodeboxResult(context.workload, { runId: context.runId, fixtureOnly: precomputedCodeboxResultIsFixtureOnly(context.workload) });
	return buildWordPressFuzzRunnerSummary({ ...context, codeboxResult });
}

async function runWordPressFuzzRunnerResult(options = {}) {
	const context = buildWordPressFuzzRunnerContext(options);
	const codeboxResult = await resolveCodeboxResult(context, options);
	return buildWordPressFuzzRunnerSummary({ ...context, codeboxResult });
}

function buildWordPressFuzzRunnerContext(options = {}) {
	const env = options.env || readWordPressFuzzRunnerEnv();
	const workloadPath = requiredString(env.workloadPath, 'HOMEBOY_FUZZ_WORKLOAD_PATH');
	const workload = options.workload || readJsonFile(workloadPath);
	const packageRoot = options.packageRoot || options.package_root || packageRootFromManifestPath(workloadPath);
	const runId = requiredString(env.runId || workload.run_id || workload.runId || workload.id, 'HOMEBOY_FUZZ_RUN_ID');
	const workloadId = env.workloadId || workload.workload_id || workload.workloadId || workload.id || null;
	const seed = env.seed || workload.seed || null;
	const maxDuration = numericValue(env.maxDuration ?? workload.max_duration ?? workload.maxDuration);
	const plan = normalizeRunnerPlan(workload.plan || workload.fuzz_plan || workload.fuzzPlan || workload);
	const runtimeCapabilities = normalizeWordPressFuzzRuntimeCapabilities(resolveRuntimeCapabilitiesForWorkload(workload, plan));
	const instructions = fuzzSuiteInstructions({ workload, workloadId, runId });
	const wpCodeboxInput = buildWpCodeboxInput({ workload, plan, runId, workloadId, seed, maxDuration, executionRequest: env.executionRequest, instructions, runtimeCapabilities, packageRoot });
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
		packageRoot,
		runtimeRequirements,
		runtimeTaskRequest,
		taskRequest,
	};
}

function packageRootFromManifestPath(manifestPath) {
	const directory = path.dirname(String(manifestPath || ''));
	return ['fuzz', 'bench', 'manifests', 'tools'].includes(path.basename(directory)) ? path.dirname(directory) : directory;
}

function resolveRuntimeCapabilitiesForWorkload(workload = {}, plan = {}) {
	const declared = workload.runtime_capabilities || workload.runtimeCapabilities || workload.runtime_profile?.fuzz_runtime_capabilities || workload.runtimeProfile?.fuzzRuntimeCapabilities;
	if (hasRuntimeCapabilities(declared)) {
		return declared;
	}

	if (!isWpCodeboxBackedWorkload(workload)) {
		return [];
	}

	const planCapabilities = requiredWpCodeboxContractsForFuzzPlan(plan).capabilities;
	const caseCapabilities = normalizeArray(workload.cases).flatMap((testCase) => requiredWpCodeboxContractsForFuzzCase(testCase).capabilities);

	return {
		capabilities: [...new Set([...planCapabilities, ...caseCapabilities])].sort(),
		metadata: { source: 'wp-codebox-command-manifest' },
	};
}

function hasRuntimeCapabilities(value) {
	return normalizeWordPressFuzzRuntimeCapabilities(value || []).capabilities.length > 0;
}

function isWpCodeboxBackedWorkload(workload = {}) {
	const runner = workload.wordpress_runner || workload.wordpressRunner || workload.runner || workload.workload?.runner || workload.metadata?.wordpress_runner || workload.metadata?.wordpressRunner;
	const entry = workload.workload?.entry || workload.entry || workload.metadata?.entry;
	return String(runner || '').toLowerCase() === 'wp-codebox'
		|| String(entry || '').startsWith('wp-codebox/')
		|| workload.schema === 'homeboy/fuzz-workload/v1';
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
	codeboxResult,
}) {
	codeboxResult = withHomeboyRequiredFuzzArtifacts(codeboxResult, { workloadId });
	const coverage = aggregateCoverage(workload, codeboxResult);
	const status = normalizeRunnerStatus(codeboxResult, coverage);
	const homeboyFuzzResultEnvelope = buildHomeboyFuzzResultEnvelope({ runId, workloadId, seed, maxDuration, workload, plan, codeboxResult, status, runtimeTaskRequest, taskRequest });
	const homeboyFuzzCampaign = buildHomeboyFuzzCampaign({ runId, workloadId, plan, codeboxResult, status, homeboyFuzzResultEnvelope });

	return stripUndefined({
		schema: WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
		status,
		succeeded: !['failed', 'errored', 'error', 'skipped', 'unsupported', 'not_executed'].includes(String(status).toLowerCase()),
		run_id: runId,
		workload_id: workloadId,
		seed,
		max_duration_seconds: maxDuration,
		plan_id: plan.id,
		wp_codebox_input: wpCodeboxInput,
		wordpress_fuzz_runtime_capabilities: runtimeCapabilities,
		wp_codebox_runtime_requirements: runtimeRequirements,
		wp_codebox_task_request: taskRequest,
		fuzz_runtime_task_request: runtimeTaskRequest,
		fuzz_runtime_task_result: codeboxResult.runtime_task_result,
		wp_codebox_result: codeboxResult,
		observation: codeboxResult.observation,
		coverage,
		observation_set: codeboxResult.observation_set,
		hotspot_summary: codeboxResult.hotspot_summary || coverage?.hotspot_summary,
		homeboy_fuzz_campaign: homeboyFuzzCampaign,
		homeboy_fuzz_result_envelope: homeboyFuzzResultEnvelope,
		metadata: objectOrUndefined(workload.metadata),
	});
}

async function resolveCodeboxResult(context, options = {}) {
	const runner = options.runFuzzSuite || options.runRuntimeTask || options.runTask;
	const runtimeDescriptor = typeof runner === 'function' ? {} : await readWpCodeboxFuzzRuntimeDescriptor({ ...options, env: context.env });
	return runWpCodeboxFuzzSuite({
		...options,
		...runtimeDescriptor,
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

async function readWpCodeboxFuzzRuntimeDescriptor(options = {}) {
	if (hasExplicitWpCodeboxFuzzRuntimeContract(options)) {
		return {};
	}

	const client = createCodeboxClient(options);
	let result;
	try {
		result = await client.runPublicCliCommand(['runtime', 'descriptor', '--json'], options);
	} catch (error) {
		return blockedWpCodeboxFuzzRuntimeDescriptor(client, error.message);
	}
	if (result.status !== 0) {
		return blockedWpCodeboxFuzzRuntimeDescriptor(client, `exited with status ${result.status}.`, result);
	}

	let descriptor;
	try {
		descriptor = JSON.parse(result.stdout);
	} catch {
		return blockedWpCodeboxFuzzRuntimeDescriptor(client, 'did not return valid JSON.', result);
	}
	if (!objectOrUndefined(descriptor) || descriptor.schema !== 'wp-codebox/runtime-descriptor/v1') {
		return blockedWpCodeboxFuzzRuntimeDescriptor(client, 'did not return a wp-codebox/runtime-descriptor/v1 document.', result);
	}

	const runtimeContractManifest = objectOrUndefined(descriptor.contractManifest);
	const publicCliReadiness = objectOrUndefined(runtimeContractManifest?.readiness?.wordpressRuntime);
	if (!runtimeContractManifest || !publicCliReadiness) {
		return blockedWpCodeboxFuzzRuntimeDescriptor(client, 'did not declare contractManifest.readiness.wordpressRuntime.', result, descriptor);
	}

	return { runtimeDescriptor: descriptor, runtimeContractManifest, publicCliReadiness };
}

function hasExplicitWpCodeboxFuzzRuntimeContract(options = {}) {
	return Boolean(
		options.runtimeContractManifest
		|| options.runtime_contract_manifest
		|| options.publicCliReadiness
		|| options.public_cli_readiness
	);
}

function blockedWpCodeboxFuzzRuntimeDescriptor(client, detail, result = {}, descriptor) {
	return {
		runtimeDescriptor: descriptor,
		runtimeContractManifest: {},
		publicCliReadiness: {
			schema: 'wp-codebox/fuzz-runner-readiness/v1',
			status: 'blocked',
			mode: 'runtime-backed',
			command_available: false,
			diagnostics: [{
				severity: 'error',
				code: 'wp_codebox_runtime_descriptor_unavailable',
				message: `WP Codebox runtime descriptor from ${wpCodeboxRuntimeDescriptorBin(client)} ${detail}`,
				stderr: result.stderr || undefined,
				stdout: result.stdout || undefined,
			}],
		},
	};
}

function wpCodeboxRuntimeDescriptorBin(client) {
	try {
		return client.publicCliBin();
	} catch {
		return 'the configured WP Codebox binary';
	}
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

function buildWpCodeboxInput({ workload, plan, runId, workloadId, seed, maxDuration, executionRequest, instructions, runtimeCapabilities, packageRoot }) {
	const homeboyFuzzWorkload = homeboyFuzzWorkloadForCodebox(workload, { workloadId });
	return wpCodeboxFuzzSuiteInput({
		id: runId,
		goal: instructions,
		executionRequest,
		target: workload.target || { type: 'wordpress', workload_id: workloadId },
		homeboyFuzzWorkload,
		packageRoot,
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
		fixture_plan: workload.fixture_plan || workload.fixturePlan || workload.metadata?.fixture_plan || workload.metadata?.fixturePlan,
		rest_mutation_opt_ins: workload.rest_mutation_opt_ins || workload.restMutationOptIns || workload.rest_mutation_opt_in || workload.restMutationOptIn || workload.metadata?.rest_mutation_opt_ins || workload.metadata?.restMutationOptIns,
		metadata: stripUndefined({ ...(workload.metadata || {}), runner: WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA, runtime_capabilities: runtimeCapabilities, workload: stripUndefined({ id: workloadId }) }),
	});
}

function homeboyFuzzWorkloadForCodebox(workload = {}, { workloadId } = {}) {
	if (workload.schema === 'homeboy/fuzz-workload/v1') {
		return workload;
	}
	if (!isWpCodeboxBackedWorkload(workload)) {
		return undefined;
	}
	const workloadPath = workload.workload?.path || workload.workload_path || workload.workloadPath || workload.metadata?.workload_path || workload.metadata?.workloadPath;
	const workloadDefinition = objectOrUndefined(workload.workload?.definition || workload.workload_definition || workload.workloadDefinition || workload.metadata?.workload_definition || workload.metadata?.workloadDefinition);
	if (!workloadDefinition && (typeof workloadPath !== 'string' || workloadPath.trim() === '')) {
		return undefined;
	}
	return stripUndefined({
		...workload,
		schema: 'homeboy/fuzz-workload/v1',
		id: workload.id || workloadId,
		workload: stripUndefined({
			...(objectOrUndefined(workload.workload) || {}),
			path: workloadPath,
			definition: workloadDefinition,
			type: workload.workload?.type || workload.workload_type || workload.workloadType || workload.metadata?.workload_type || workload.metadata?.workloadType,
			entry: workload.workload?.entry || workload.entry || workload.metadata?.entry || workload.id || workloadId,
			runner: workload.workload?.runner || workload.runner || workload.metadata?.wordpress_runner || workload.metadata?.wordpressRunner,
		}),
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

function wpCodeboxRuntimeRequirementsFromWorkload(workload = {}, options = {}) {
	return buildWpCodeboxFuzzRuntimeRequirements({
		workload,
		env: options.env,
	});
}

function buildWpCodeboxFuzzRuntimeRequirements({ workload = {}, env = {} } = {}) {
	const context = objectOrUndefined(workload.metadata?.homeboy_runtime_context || workload.metadata?.homeboyRuntimeContext);
	const workloadRoot = nonEmptyString(env?.wpCodeboxFuzzWorkloadRoot || env?.WP_CODEBOX_FUZZ_WORKLOAD_ROOT);
	const target = objectOrUndefined(workload.target) || {};
	if (target.type === 'wordpress-core') {
		return stripUndefined({
			wordpress_directory: nonEmptyString(target.wordpress_directory),
			runtime_mounts: workloadRoot ? [{ source: workloadRoot, target: workloadRoot, mode: 'readonly' }] : undefined,
			runtime_env: workloadRoot ? { WP_CODEBOX_FUZZ_WORKLOAD_ROOT: workloadRoot } : undefined,
			metadata: stripUndefined({
				homeboy_runtime_context_schema: context?.schema,
				rig_id: context?.rig_id,
			}),
		});
	}
	const components = objectOrUndefined(context?.components);
	const componentId = workload.target?.component
		|| workload.metadata?.fixture?.component
		|| workload.metadata?.fixture?.plugin
		|| workload.target?.slug;
	const component = componentId && components ? componentFromContext(components, componentId) : undefined;
	const source = componentPathOverride(env.componentPathOverrides, componentId, context?.rig_id)
		|| component?.path
		|| component?.source;
	if ((!componentId || typeof source !== 'string' || source.trim() === '') && !workloadRoot) {
		return undefined;
	}
	const activation = workload.metadata?.fixture?.activation || firstCasePluginActivation(workload);
	const checkoutRoot = componentPathOverride(env.componentCheckoutRootOverrides, componentId, context?.rig_id)
		|| component?.checkout_root
		|| component?.checkoutRoot
		|| component?.extensions?.wordpress?.checkout_root
		|| component?.extensions?.wordpress?.checkoutRoot;
	const pluginRequirement = buildWpCodeboxFuzzPluginRequirement({ workload, componentId, source, activation, context, checkoutRoot, env });
	return {
		extra_plugins: pluginRequirement ? [pluginRequirement.extraPlugin] : undefined,
		runtime_mounts: workloadRoot ? [{ source: workloadRoot, target: workloadRoot, mode: 'readonly' }] : undefined,
		runtime_env: workloadRoot ? { WP_CODEBOX_FUZZ_WORKLOAD_ROOT: workloadRoot } : undefined,
		metadata: stripUndefined({
			homeboy_runtime_context_schema: context?.schema,
			rig_id: context?.rig_id,
		}),
	};
}

function buildWpCodeboxFuzzPluginRequirement({ workload = {}, componentId, source, activation, context = {}, checkoutRoot, env = {} } = {}) {
	if (!componentId || typeof source !== 'string' || source.trim() === '') {
		return undefined;
	}
	const slug = workload.target?.slug || componentId;
	const component = componentFromContext(context.components, componentId);
	const wordpressExtension = objectOrUndefined(component?.extensions?.wordpress);
	const sourceSubpath = nonEmptyString(wordpressExtension?.wp_codebox_source_subpath);
	const sourceLayout = wpCodeboxSourceLayout({ source, sourceSubpath, wordpressExtension, checkoutRoot, env });
	const mountSlug = nonEmptyString(wordpressExtension?.wp_codebox_mount_slug) || slug;
	const pluginFile = wpCodeboxPluginFile({ activation, sourceLayout, wordpressExtension, mountSlug });
	return {
		extraPlugin: stripUndefined({
			slug,
			source: sourceLayout.sourceRoot,
			sourceSubpath: sourceLayout.sourceSubpath,
			mountSlug,
			pluginFile,
			loadAs: 'plugin',
			metadata: stripUndefined({
				component: componentId,
				rig_id: context.rig_id,
				activation: activation ? 'fuzz-suite-setup-step' : undefined,
			}),
		}),
	};
}

function wpCodeboxPluginFile({ activation, sourceLayout = {}, wordpressExtension = {}, mountSlug } = {}) {
	const configured = nonEmptyString(wordpressExtension?.wp_codebox_plugin_file);
	if (configured) {
		if (sourceLayout.sourceSubpath && configured.startsWith(`${sourceLayout.sourceSubpath}/`)) {
			return joinRelativePath(mountSlug || path.basename(sourceLayout.sourceSubpath), configured.slice(sourceLayout.sourceSubpath.length + 1));
		}
		return configured.includes('/') || configured.includes('\\') || !sourceLayout.sourceSubpath
			? normalizePathSeparators(configured)
			: joinRelativePath(sourceLayout.sourceSubpath, configured);
	}

	const normalizedActivation = nonEmptyString(activation);
	if (!normalizedActivation) {
		return undefined;
	}
	if (sourceLayout.sourceSubpath && normalizedActivation.startsWith(`${sourceLayout.sourceSubpath}/`)) {
		return joinRelativePath(mountSlug || path.basename(sourceLayout.sourceSubpath), normalizedActivation.slice(sourceLayout.sourceSubpath.length + 1));
	}
	if (sourceLayout.sourceIsPluginRoot || !sourceLayout.sourceSubpath || normalizedActivation.startsWith(`${mountSlug}/`)) {
		return normalizePathSeparators(normalizedActivation);
	}
	return joinRelativePath(mountSlug || path.basename(sourceLayout.sourceSubpath), path.basename(normalizedActivation));
}

function joinRelativePath(...parts) {
	return parts
		.map((part) => normalizePathSeparators(part).replace(/^\/+|\/+$/g, ''))
		.filter(Boolean)
		.join('/');
}

function normalizePathSeparators(value) {
	return String(value || '').replace(/\\+/g, '/');
}

function wpCodeboxSourceLayout({ source, sourceSubpath, wordpressExtension, checkoutRoot, env = {} } = {}) {
	const normalizedSubpath = nonEmptyString(sourceSubpath);
	if (normalizedSubpath && source.endsWith(`/${normalizedSubpath}`)) {
		return {
			sourceRoot: source.slice(0, -normalizedSubpath.length - 1),
			sourceSubpath: normalizedSubpath,
			sourceIsPluginRoot: true,
		};
	}
	const normalizedCheckoutRoot = nonEmptyString(checkoutRoot);
	if (normalizedCheckoutRoot && source.startsWith(`${normalizedCheckoutRoot}/`)) {
		return {
			sourceRoot: normalizedCheckoutRoot,
			sourceSubpath: source.slice(normalizedCheckoutRoot.length + 1),
			sourceIsPluginRoot: true,
		};
	}

	const configured = resolvedMetadataString(wordpressExtension?.wp_codebox_source_root, env);
	if (configured && !configured.startsWith('~/')) {
		return {
			sourceRoot: configured,
			sourceSubpath: normalizedSubpath,
		};
	}

	if (normalizedSubpath) {
		return {
			sourceRoot: source,
			sourceSubpath: normalizedSubpath,
			sourceIsPluginRoot: false,
		};
	}

	return {
		sourceRoot: source,
		sourceIsPluginRoot: true,
	};
}

function resolvedMetadataString(value, env = {}) {
	const normalized = nonEmptyString(value);
	if (!normalized) {
		return undefined;
	}
	const resolved = normalized.replace(/\$\{env\.([A-Za-z_][A-Za-z0-9_]*)\}/g, (match, name) => {
		const replacement = nonEmptyString(env[name]);
		return replacement || match;
	});
	return resolved.includes('${env.') ? undefined : resolved;
}

function componentPathOverridesFromEnv(env = {}, prefix = '') {
	const overrides = {};
	for (const [key, value] of Object.entries(env)) {
		if (!key.startsWith(prefix) || typeof value !== 'string' || value.trim() === '') {
			continue;
		}
		const suffix = key.slice(prefix.length);
		const normalizedKey = normalizedComponentId(suffix);
		if (!normalizedKey) {
			continue;
		}
		overrides[normalizedKey] = value.trim();
	}
	return Object.keys(overrides).length > 0 ? overrides : undefined;
}

function componentFromContext(components = {}, componentId) {
	const normalizedId = normalizedComponentId(componentId);
	for (const [key, value] of Object.entries(objectOrUndefined(components) || {})) {
		if (key === componentId || normalizedComponentId(key) === normalizedId) {
			return objectOrUndefined(value);
		}
	}
	return undefined;
}

function componentPathOverride(overrides = {}, componentId, rigId) {
	const normalizedOverrides = objectOrUndefined(overrides) || {};
	const componentKey = normalizedComponentId(componentId);
	const rigKey = normalizedComponentId(rigId);
	return normalizedOverrides[componentKey]
		|| (rigKey ? normalizedOverrides[`${rigKey}-${componentKey}`] : undefined);
}

function normalizedComponentId(value) {
	return nonEmptyString(value)?.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
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
		if (context.fixtureOnly !== true) {
			return normalizeWpCodeboxFuzzSuiteResult({
				schema: 'wp-codebox/fuzz-suite-result/v1',
				request_id: context.runId,
				status: 'unsupported',
				diagnostics: [
					{
						severity: 'error',
						code: 'wp_codebox_precomputed_fuzz_result_not_fixture_only',
						message: 'Embedded WP Codebox fuzz results are accepted only for explicit fixture-only workloads. Production fuzz execution must consume the WP Codebox runtime manifest and fuzz readiness contract.',
					},
				],
				metadata: { unsupported: true, precomputed_result_blocked: true },
			});
		}
		return normalizeWpCodeboxFuzzSuiteResult(result);
	}
	return normalizeWpCodeboxFuzzSuiteResult({
		schema: 'wp-codebox/fuzz-suite-result/v1',
		request_id: context.runId,
		status: 'unsupported',
		diagnostics: [
			{
				severity: 'warning',
				code: 'wp_codebox_fuzz_suite_execution_unsupported',
				message: 'WP Codebox exposes the public fuzz suite contract, but no merged execution API was available to this runner. Provide wp_codebox_suite_result in the workload or install a Codebox runtime that executes wp-codebox/run-fuzz-suite.',
			},
		],
	});
}

function precomputedCodeboxResult(workload = {}) {
	return workload.wp_codebox_result || workload.wpCodeboxResult || workload.wp_codebox_suite_result || workload.wpCodeboxSuiteResult || workload.result;
}

function precomputedCodeboxResultIsFixtureOnly(workload = {}) {
	return workload.fixture_only === true
		|| workload.fixtureOnly === true
		|| workload.metadata?.fixture_only === true
		|| workload.metadata?.fixtureOnly === true;
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
	const status = codeboxResult.status || 'succeeded';
	const normalized = String(status).toLowerCase();
	if (['skipped', 'unsupported', 'not_executed'].includes(normalized)) {
		return normalized;
	}
	if (hasCoverageFailures(coverage) || codeboxResult.succeeded === false) {
		return 'failed';
	}
	return status;
}

function buildHomeboyFuzzCampaign({ runId, workloadId, plan, codeboxResult, status, homeboyFuzzResultEnvelope }) {
	const diagnostics = normalizeArray(codeboxResult?.failures || codeboxResult?.metadata?.diagnostics || codeboxResult?.diagnostics);
	return stripUndefined({
		schema: HOMEBOY_FUZZ_CAMPAIGN_SCHEMA,
		version: HOMEBOY_FUZZ_CONTRACT_VERSION,
		id: runId,
		title: `WordPress fuzz campaign ${runId}`,
		safety_class: deriveHomeboyFuzzSafetyClass(plan),
		artifacts: homeboyFuzzCampaignArtifacts(codeboxResult),
		metadata: stripUndefined({
			workload_id: workloadId,
			plan_id: plan?.id,
			status,
			success: codeboxResult?.succeeded,
			wp_codebox_result_schema: codeboxResult?.result_schema,
			diagnostics: diagnostics.length > 0 ? diagnostics : undefined,
			artifact_refs: reportedCodeboxArtifactRefs(codeboxResult),
			observation_set: codeboxResult?.observation_set,
			hotspot_summary: codeboxResult?.hotspot_summary,
			observation: codeboxResult?.observation,
			wordpress_fuzz_result: codeboxResult?.wordpress_fuzz_result,
			fuzz_result_envelope: homeboyFuzzResultEnvelope,
		}),
	});
}

function reportedCodeboxArtifactRefs(codeboxResult = {}) {
	return normalizeArray(codeboxResult.artifacts).filter((artifact) => objectOrUndefined(artifact) && artifact.payload === undefined);
}

function buildHomeboyFuzzResultEnvelope({ runId, workloadId, seed, maxDuration, workload, plan, codeboxResult, status, runtimeTaskRequest, taskRequest }) {
	const artifacts = normalizeArray(codeboxResult?.artifacts);
	const requiredArtifacts = requiredFuzzArtifactStatuses({ artifacts, runtimeTaskRequest, taskRequest, workload });
	const failures = normalizeArray(codeboxResult?.failures || codeboxResult?.metadata?.diagnostics || codeboxResult?.diagnostics);
	const dispatchIdentity = fuzzDispatchIdentityPassthrough({ codeboxResult, runtimeTaskRequest, taskRequest, workload });
	return stripUndefined({
		schema: HOMEBOY_FUZZ_RESULT_ENVELOPE_SCHEMA,
		version: HOMEBOY_FUZZ_CONTRACT_VERSION,
		id: codeboxResult?.request_id || runId,
		status,
		succeeded: codeboxResult?.succeeded,
		campaign: stripUndefined({
			id: runId,
			workload_id: workloadId,
			plan_id: plan?.id,
			safety_class: deriveHomeboyFuzzSafetyClass(plan),
			seed,
			max_duration_seconds: maxDuration,
			metadata: objectOrUndefined(workload?.metadata),
		}),
		result: stripUndefined({
			provider: 'wp-codebox',
			provider_result_schema: codeboxResult?.result_schema,
			wordpress_fuzz_result: codeboxResult?.wordpress_fuzz_result,
			observation: codeboxResult?.observation,
			observation_set: codeboxResult?.observation_set,
			hotspot_summary: codeboxResult?.hotspot_summary,
		}),
		artifacts,
		gates: stripUndefined({
			required_artifacts: requiredArtifacts.length > 0 ? requiredArtifacts : undefined,
			failures: failures.length > 0 ? failures : undefined,
		}),
		dispatch: fuzzDispatchIdentity({ codeboxResult, runtimeTaskRequest, taskRequest }),
		dispatch_identity: dispatchIdentity,
	});
}

function withHomeboyRequiredFuzzArtifacts(codeboxResult = {}, options = {}) {
	if (!codeboxFuzzResultSucceeded(codeboxResult)) {
		return codeboxResult;
	}
	const artifacts = normalizeArray(codeboxResult.artifacts);
	const workloadId = options.workloadId || codeboxResult.workload_id || codeboxResult.workloadId || 'wordpress-fuzz';
	const normalizedArtifacts = [
		...artifacts,
		...homeboyRequiredFuzzArtifacts({ codeboxResult, workloadId }),
	];
	return {
		...codeboxResult,
		artifacts: dedupeFuzzArtifacts(normalizedArtifacts),
	};
}

function codeboxFuzzResultSucceeded(codeboxResult = {}) {
	const status = String(codeboxResult.status || '').toLowerCase();
	return codeboxResult.succeeded === true || ['succeeded', 'success', 'passed', 'ok'].includes(status);
}

function homeboyRequiredFuzzArtifacts({ codeboxResult = {}, workloadId = 'wordpress-fuzz' } = {}) {
	const normalizedResult = codeboxResult.wordpress_fuzz_result || codeboxResult.wordpressFuzzResult;
	const coverageSummary = codeboxResult.coverage_summary || codeboxResult.coverageSummary;
	const cases = normalizeArray(normalizedResult?.cases || codeboxResult.cases);
	return [
		{
			name: 'wp-codebox-fuzz-suite-result',
			role: 'codebox_result',
			semantic_key: 'fuzz.result.normalized',
			path: 'files/wp-codebox-fuzz-suite-result.json',
			content_type: 'application/json',
			payload: normalizedResult || codeboxResult,
		},
		{
			name: 'wordpress-fuzz-coverage',
			role: 'coverage_summary_gaps',
			semantic_key: 'fuzz.coverage',
			path: 'files/wordpress-fuzz-coverage.json',
			content_type: 'application/json',
			payload: coverageSummary || codeboxResult.coverage || { schema: 'homeboy/wordpress-fuzz-coverage-summary/v1', status: codeboxResult.status },
		},
		{
			name: 'coverage-summary',
			role: 'coverage_summary',
			semantic_key: 'fuzz.coverage.summary',
			path: 'files/coverage-summary.json',
			content_type: 'application/json',
			payload: coverageSummary || { schema: 'homeboy/wordpress-fuzz-coverage-summary/v1', status: codeboxResult.status },
		},
		{
			name: 'case-log',
			role: 'case_log',
			semantic_key: 'fuzz.case.log',
			path: 'files/case-log.jsonl',
			content_type: 'application/jsonl',
			payload: cases.length > 0 ? cases : [{ id: codeboxResult.request_id, status: codeboxResult.status }],
		},
		{
			name: 'replay-data',
			role: 'replay_data',
			semantic_key: 'fuzz.replay.data',
			path: 'files/replay-data.json',
			content_type: 'application/json',
			payload: {
				schema: 'homeboy/fuzz-replay-data/v1',
				request_id: codeboxResult.request_id,
				cases: cases.map((testCase) => ({ id: testCase.id, status: testCase.status })),
			},
		},
		{
			name: workloadId,
			role: 'fuzz_report',
			semantic_key: 'fuzz.report',
			path: `${workloadId}/${workloadId}.json`,
			content_type: 'application/json',
			payload: normalizedResult || codeboxResult,
		},
	].filter((artifact) => artifact.payload !== undefined);
}

function dedupeFuzzArtifacts(artifacts = []) {
	const seen = new Set();
	return normalizeArray(artifacts).filter((artifact) => {
		const key = [artifact.name, artifact.role, artifact.semantic_key || artifact.semanticKey, artifact.path].filter(Boolean).join(':');
		if (!key || seen.has(key)) {
			return false;
		}
		seen.add(key);
		return true;
	});
}

function requiredFuzzArtifactStatuses({ artifacts = [], runtimeTaskRequest = {}, taskRequest = {}, workload = {} } = {}) {
	const declarations = dedupeRequiredArtifactDeclarations([
		...normalizeArray(runtimeTaskRequest.artifact_declarations),
		...normalizeArray(taskRequest.artifact_declarations),
		...normalizeArray(workload?.artifacts?.expected),
		...normalizeArray(workload?.artifacts?.required),
		...normalizeArray(workload?.cases).flatMap((entry) => normalizeArray(entry?.artifacts).filter((artifact) => artifact?.required === true)),
	]);
	return declarations.map((declaration) => stripUndefined({
		name: declaration.name,
		role: declaration.role || declaration.kind,
		semantic_key: declaration.semantic_key || declaration.semanticKey || declaration.metadata?.semantic_key,
		schema: declaration.schema || declaration.metadata?.schema,
		path: declaration.path,
		required: declaration.required !== false,
		status: artifacts.some((artifact) => fuzzArtifactMatchesDeclaration(artifact, declaration)) ? 'present' : 'missing',
	}));
}

function dedupeRequiredArtifactDeclarations(declarations = []) {
	const seen = new Set();
	return declarations.filter((declaration) => {
		if (!objectOrUndefined(declaration)) {
			return false;
		}
		if (declaration.required === false) {
			return false;
		}
		const key = [
			declaration.name,
			declaration.path,
			declaration.semantic_key || declaration.semanticKey || declaration.metadata?.semantic_key,
			declaration.schema || declaration.metadata?.schema,
		].filter(Boolean).join(':');
		if (!key || seen.has(key)) {
			return false;
		}
		seen.add(key);
		return true;
	});
}

function fuzzArtifactMatchesDeclaration(artifact = {}, declaration = {}) {
	const declarationSemanticKey = declaration.semantic_key || declaration.semanticKey || declaration.metadata?.semantic_key;
	const artifactSemanticKey = artifact.semantic_key || artifact.semanticKey || artifact.metadata?.semantic_key;
	const declarationSchema = declaration.schema || declaration.metadata?.schema;
	const artifactSchema = artifact.schema || artifact.metadata?.schema;
	return Boolean(
		(declaration.name && artifact.name === declaration.name)
		|| (declaration.path && artifact.path === declaration.path)
		|| (declarationSemanticKey && artifactSemanticKey === declarationSemanticKey)
		|| (declarationSchema && artifactSchema === declarationSchema)
	);
}

function fuzzDispatchIdentity({ codeboxResult = {}, runtimeTaskRequest = {}, taskRequest = {} } = {}) {
	const runtimeTaskResult = objectOrUndefined(codeboxResult.runtime_task_result) || {};
	const provider = runtimeTaskResult.provider || runtimeTaskRequest.provider;
	const taskId = runtimeTaskResult.task_id || runtimeTaskRequest.task_id || taskRequest.task_id || codeboxResult.request_id;
	if (!taskId && !provider?.id && !taskRequest.executor?.runtime) {
		return undefined;
	}
	return stripUndefined({
		task_id: taskId,
		provider: provider?.id || provider,
		runtime: taskRequest.executor?.runtime || runtimeTaskRequest.provider_metadata?.wp_codebox?.runtime,
		ability: taskRequest.executor?.config?.runtime_task?.ability || runtimeTaskRequest.provider_metadata?.wp_codebox?.ability,
	});
}

function fuzzDispatchIdentityPassthrough({ codeboxResult = {}, runtimeTaskRequest = {}, taskRequest = {}, workload = {} } = {}) {
	return firstObject(
		codeboxResult.dispatch_identity,
		codeboxResult.dispatchIdentity,
		codeboxResult.metadata?.dispatch_identity,
		codeboxResult.metadata?.dispatchIdentity,
		runtimeTaskRequest.dispatch_identity,
		runtimeTaskRequest.dispatchIdentity,
		runtimeTaskRequest.metadata?.dispatch_identity,
		runtimeTaskRequest.metadata?.dispatchIdentity,
		runtimeTaskRequest.input?.dispatch_identity,
		runtimeTaskRequest.input?.dispatchIdentity,
		runtimeTaskRequest.input?.metadata?.dispatch_identity,
		runtimeTaskRequest.input?.metadata?.dispatchIdentity,
		taskRequest.dispatch_identity,
		taskRequest.dispatchIdentity,
		taskRequest.inputs?.dispatch_identity,
		taskRequest.inputs?.dispatchIdentity,
		taskRequest.executor?.config?.runtime_task?.input?.dispatch_identity,
		taskRequest.executor?.config?.runtime_task?.input?.dispatchIdentity,
		taskRequest.executor?.config?.runtime_task?.input?.metadata?.dispatch_identity,
		taskRequest.executor?.config?.runtime_task?.input?.metadata?.dispatchIdentity,
		workload.dispatch_identity,
		workload.dispatchIdentity,
		workload.metadata?.dispatch_identity,
		workload.metadata?.dispatchIdentity,
	);
}

function homeboyFuzzCampaignArtifacts(codeboxResult = {}) {
	return normalizeArray(codeboxResult?.wordpress_fuzz_result?.artifacts || codeboxResult?.wordpressFuzzResult?.artifacts || codeboxResult?.artifacts)
		.map(homeboyFuzzCampaignArtifact)
		.filter(Boolean);
}

function homeboyFuzzCampaignArtifact(artifact = {}) {
	if (!objectOrUndefined(artifact)) {
		return null;
	}
	const id = artifact.id || artifact.name || artifact.role || artifact.kind;
	const kind = artifact.kind || artifact.role || artifact.name || artifact.id;
	if (!id || !kind) {
		return null;
	}
	return stripUndefined({
		schema: 'homeboy/fuzz-artifact/v1',
		id: String(id),
		kind: String(kind),
		metadata: stripUndefined({
			name: artifact.name,
			role: artifact.role,
			semantic_key: artifact.semantic_key || artifact.semanticKey,
			content_type: artifact.content_type || artifact.contentType,
			schema: artifact.schema || artifact.metadata?.schema,
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

function writeHomeboyFuzzArtifactFiles(artifactRoot, result = {}) {
	if (!artifactRoot) {
		return [];
	}
	const files = [];
	const hotspotSummary = result.hotspot_summary || result.wp_codebox_result?.hotspot_summary || result.coverage?.hotspot_summary || rawWpCodeboxHotspotArtifact(result);
	if (hotspotSummary) {
		files.push(writeJsonArtifactFile({ artifactRoot, relativePath: 'files/wordpress-hotspots.json', payload: hotspotSummary }));
	}
	for (const artifact of normalizeArray(result.wp_codebox_result?.artifacts || result.artifacts)) {
		if (!artifact.path || artifact.payload === undefined) {
			continue;
		}
		files.push(writeFuzzArtifactPayload({ artifactRoot, artifact }));
	}
	return files;
}

function rawWpCodeboxHotspotArtifact(result = {}) {
	return result.wp_codebox_result?.wordpress_fuzz_result?.metadata?.artifacts?.wordpressHotspots
		|| result.wp_codebox_result?.metadata?.artifacts?.wordpressHotspots;
}

function writeJsonArtifactFile({ artifactRoot, relativePath, payload }) {
	const filePath = path.join(artifactRoot, relativePath);
	fs.mkdirSync(path.dirname(filePath), { recursive: true });
	fs.writeFileSync(filePath, `${JSON.stringify(payload, null, 2)}\n`);
	return { path: filePath, relative_path: relativePath };
}

function writeFuzzArtifactPayload({ artifactRoot, artifact }) {
	if (artifact.content_type === 'application/jsonl' || artifact.contentType === 'application/jsonl') {
		const filePath = path.join(artifactRoot, artifact.path);
		fs.mkdirSync(path.dirname(filePath), { recursive: true });
		const rows = normalizeArray(artifact.payload).map((entry) => JSON.stringify(entry)).join('\n');
		fs.writeFileSync(filePath, `${rows}\n`);
		return { path: filePath, relative_path: artifact.path };
	}
	return writeJsonArtifactFile({ artifactRoot, relativePath: artifact.path, payload: artifact.payload });
}

function readJsonFile(filePath) {
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function readFuzzExecutionRequest(filePath) {
	if (filePath === undefined) {
		return undefined;
	}
	const normalizedPath = typeof filePath === 'string' ? filePath.trim() : '';
	if (!normalizedPath) {
		throw new Error('HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE must name a readable JSON file when supplied.');
	}
	let source;
	try {
		source = fs.readFileSync(normalizedPath, 'utf8');
	} catch (error) {
		throw new Error(`Unable to read HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE at ${normalizedPath}: ${error.message}`);
	}
	try {
		return JSON.parse(source);
	} catch (error) {
		throw new Error(`Invalid JSON in HOMEBOY_FUZZ_EXECUTION_REQUEST_FILE at ${normalizedPath}: ${error.message}`);
	}
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

function firstObject(...values) {
	return values.find(objectOrUndefined);
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
	HOMEBOY_FUZZ_RESULT_ENVELOPE_SCHEMA,
	WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
	buildWordPressFuzzRunnerResult,
	runWordPressFuzzRunnerResult,
	writeHomeboyFuzzArtifactFiles,
	writeHomeboyFuzzResultsFile,
	readWordPressFuzzRunnerEnv,
};
