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
	WORDPRESS_FUZZ_RESULT_SCHEMA,
	normalizeWordPressFuzzResult,
} = require('./wordpress-fuzz-schemas');

const {
	wordpressRuntimeTaskRequest,
} = require('./wordpress-runtime-task-planner');
const {
	buildWordPressFuzzRuntimeTaskRequest,
	fuzzHotspotSummaryFromObservationSet,
	normalizeFuzzObservationSet,
	normalizeFuzzHotspotSummary,
	normalizeWordPressFuzzRuntimeTaskResult,
} = require('./wordpress-fuzz-runtime-task');

const {
	normalizeWordPressFuzzRuntimeCapabilities,
} = require('./wordpress-fuzz-runtime-capabilities');
const {
	normalizeWordPressFuzzMutationLifecycleContract,
	wordpressFuzzMutationLifecycleDiagnosticsForCase,
} = require('./wordpress-fuzz-mutation-lifecycle');
const {
	createCodeboxClient,
} = require('./codebox-client');
const {
	WP_CODEBOX_FUZZ_PUBLIC_ABILITIES,
	WP_CODEBOX_FUZZ_PUBLIC_COMMANDS,
	WP_CODEBOX_FUZZ_PUBLIC_RUNNER_MODES,
	buildWordPressFuzzCommandManifest,
	requiredWpCodeboxContractsForFuzzPlan,
} = require('./wordpress-fuzz-command-manifest');

const WP_CODEBOX_FUZZ_SUITE_SCHEMA = 'wp-codebox/fuzz-suite/v1';
const WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA = 'wp-codebox/fuzz-suite-result/v1';
const WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA = 'wp-codebox/wordpress-hotspots/v1';
const WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA = 'homeboy/wordpress-codebox-fuzz-suite-consumer/v1';
const WP_CODEBOX_FUZZ_EXECUTION_SCHEMA = 'homeboy/wp-codebox-fuzz-execution/v1';
const WP_CODEBOX_FUZZ_PREFLIGHT_SCHEMA = 'homeboy/wp-codebox-fuzz-preflight/v1';
const WP_CODEBOX_FUZZ_RUNNER_READINESS_SCHEMA = 'wp-codebox/fuzz-runner-readiness/v1';
const WORDPRESS_FUZZ_OBSERVATION_SCHEMA = 'homeboy/wordpress-fuzz-observation/v1';
const DEFAULT_FUZZ_SUITE_ABILITY = 'wp-codebox/run-fuzz-suite';
const DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY = 'wp-codebox/run-wordpress-workload';
const DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA = 'wp-codebox/wordpress-workload-run/v1';
const DEFAULT_RUNTIME_CONTRACT_MANIFEST = Object.freeze({
	schema: 'wp-codebox/runtime-contract-manifest/v1', version: 1,
	abilities: { wordpressRuntime: { runFuzzSuite: DEFAULT_FUZZ_SUITE_ABILITY, runWorkload: DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY } },
	commands: { wordpressRuntime: { runFuzzSuite: 'run-fuzz-suite', runWorkload: 'run-wordpress-workload' } },
	schemas: { wordpressRuntime: { fuzzSuite: WP_CODEBOX_FUZZ_SUITE_SCHEMA, fuzzSuiteResult: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA, workloadRun: DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA } },
	capabilities: { wordpressRuntime: { commands: ['run-fuzz-suite', 'run-wordpress-workload'], runner_modes: { 'runtime-backed': true } } },
	readiness: { wordpressRuntime: { schema: WP_CODEBOX_FUZZ_RUNNER_READINESS_SCHEMA, status: 'ready', mode: 'runtime-backed', command_available: true } },
});
const WP_CODEBOX_PUBLIC_CLI_COMMANDS = WP_CODEBOX_FUZZ_PUBLIC_COMMANDS;
const ARTIFACT_POSTPROCESS_COMMAND = 'homeboy.artifact-postprocess';
const ARTIFACT_POSTPROCESS_CONTRACT = 'homeboy/artifact-postprocess/v1';
const ARTIFACT_POSTPROCESS_COMMAND_ALIASES = new Set([ARTIFACT_POSTPROCESS_COMMAND, 'artifact-postprocess', 'homeboy.artifact_postprocess']);
const DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS = [
	'wp-codebox-fuzz-suite-result',
	'wordpress-fuzz-coverage',
	'result-envelope',
	'case-log',
	'replay-data',
	'coverage-summary',
];
const DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS = [
	{
		role: 'codebox_result',
		name: 'wp-codebox-fuzz-suite-result',
		semantic_key: 'fuzz.result.normalized',
		content_type: 'application/json',
		required: true,
	},
	{
		role: 'coverage_summary_gaps',
		name: 'wordpress-fuzz-coverage',
		semantic_key: 'fuzz.coverage',
		content_type: 'application/json',
		required: true,
	},
	{
		role: 'observation_set',
		name: 'fuzz-observation-set',
		semantic_key: 'fuzz.observation_set',
		content_type: 'application/json',
		required: false,
	},
	{
		role: 'codebox_result',
		name: 'fuzz-report',
		semantic_key: 'fuzz.report',
		content_type: 'application/json',
		required: false,
	},
	{
		role: 'replay_repro_data',
		name: 'fuzz-case-artifacts',
		semantic_key: 'fuzz.case.artifact',
		content_type: 'application/json',
		required: false,
	},
	{
		role: 'replay_repro_data',
		name: 'fuzz-repro-cases',
		semantic_key: 'fuzz.case.repro',
		required: false,
	},
	{
		role: 'result_envelope',
		name: 'result-envelope',
		semantic_key: 'fuzz.result.envelope',
		content_type: 'application/json',
		required: true,
	},
	{
		role: 'case_log',
		name: 'case-log',
		semantic_key: 'fuzz.case.log',
		content_type: 'application/jsonl',
		required: true,
	},
	{
		role: 'replay_data',
		name: 'replay-data',
		semantic_key: 'fuzz.replay.data',
		content_type: 'application/json',
		required: true,
	},
	{
		role: 'coverage_summary',
		name: 'coverage-summary',
		semantic_key: 'fuzz.coverage.summary',
		content_type: 'application/json',
		required: true,
	},
	{
		role: 'hotspot_summary',
		name: 'wordpress-hotspots',
		semantic_key: 'fuzz.hotspot.summary',
		schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
		content_type: 'application/json',
		required: false,
	},
	{
		role: 'sandbox_isolation_proof',
		name: 'sandbox-isolation-proof',
		semantic_key: 'fuzz.disposable.sandbox_isolation_proof',
		schema: 'wp-codebox/sandbox-isolation-proof/v1',
		content_type: 'application/json',
		required: false,
	},
	{
		role: 'mutation_isolation_artifact',
		name: 'mutation-isolation-artifact',
		semantic_key: 'fuzz.mutation.isolation',
		schema: 'wp-codebox/mutation-isolation-artifact/v1',
		content_type: 'application/json',
		required: false,
	},
	{
		role: 'delete_boundary_artifact',
		name: 'delete-boundary-artifact',
		semantic_key: 'fuzz.delete.boundary',
		schema: 'wp-codebox/delete-boundary-artifact/v1',
		content_type: 'application/json',
		required: false,
	},
	{
		role: 'external_http_guardrail',
		name: 'external-http-guardrail',
		semantic_key: 'fuzz.external_http.guardrail',
		content_type: 'application/jsonl',
		required: false,
	},
	{
		role: 'runtime_access',
		name: 'runtime-access',
		semantic_key: 'fuzz.runtime.access',
		content_type: 'application/json',
		required: false,
	},
];
const WP_CODEBOX_DESTRUCTIVE_READINESS_REQUIREMENTS = [
	{ key: 'disposable_runtime', label: 'disposable runtime isolation' },
	{ key: 'disposable_sandbox_boundary', label: 'disposable sandbox boundary identity' },
	{ key: 'destructive_permission', label: 'destructive permission' },
	{ key: 'mutation_boundary', label: 'mutation boundary artifact contract' },
	{ key: 'external_side_effect_guardrail', label: 'external side-effect guardrail' },
	{ key: 'artifact_export', label: 'artifact export' },
	{ key: 'teardown_discard', label: 'teardown/discard evidence' },
];
const WORDPRESS_FUZZ_POSTPROCESS_BINDING_SCHEMA = 'homeboy/wordpress-fuzz-postprocess-binding/v1';
const WORDPRESS_FUZZ_POSTPROCESS_OUTPUTS = [
	{ role: 'coverage_summary_gaps', name: 'wordpress-fuzz-coverage', semantic_key: 'fuzz.coverage', content_type: 'application/json', required: true, postprocess_output: true },
	{ role: 'hotspot_summary', name: 'homeboy-hotspot-summary', semantic_key: 'fuzz.hotspot.summary', schema: 'homeboy/fuzz-hotspot-set/v1', content_type: 'application/json', required: true, postprocess_output: true },
	{ role: 'coverage_gap_report', name: 'wordpress-fuzz-gap-report', semantic_key: 'fuzz.coverage.gap_report', schema: 'homeboy/wordpress-fuzz-coverage-gap-report/v1', content_type: 'application/json', required: true, postprocess_output: true },
	{ role: 'codebox_hotspot_artifact', name: 'wordpress-hotspots', semantic_key: 'fuzz.hotspot.codebox', schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA, content_type: 'application/json', required: true, postprocess_output: true, consumed_artifact: true },
];
const WORDPRESS_FUZZ_POSTPROCESS_BINDING = {
	schema: WORDPRESS_FUZZ_POSTPROCESS_BINDING_SCHEMA,
	version: 1,
	provider: 'wp-codebox',
	input_artifacts: [{ role: 'codebox_hotspot_artifact', name: 'wordpress-hotspots', semantic_key: 'fuzz.hotspot.codebox', schema: WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA, required: true }],
	outputs: WORDPRESS_FUZZ_POSTPROCESS_OUTPUTS,
	artifact_outputs: WORDPRESS_FUZZ_POSTPROCESS_OUTPUTS,
	proof_outputs: WORDPRESS_FUZZ_POSTPROCESS_OUTPUTS.map((artifact) => ({ name: artifact.name, role: artifact.role, semantic_key: artifact.semantic_key, schema: artifact.schema, required: artifact.required })),
};
const HOMEBOY_FUZZ_WORKLOAD_SCHEMA = 'homeboy/fuzz-workload/v1';

const FUZZ_ARTIFACT_SEMANTIC_KEYS = {
	fuzz_report: 'fuzz.report',
	fuzz_case: 'fuzz.case',
	failing_case: 'fuzz.case.failing',
	case_artifact: 'fuzz.case.artifact',
	repro_case: 'fuzz.case.repro',
	case_log: 'fuzz.case.log',
	replay_data: 'fuzz.replay.data',
	coverage_summary: 'fuzz.coverage.summary',
	coverage_gap_report: 'fuzz.coverage.gap_report',
	observation_set: 'fuzz.observation_set',
	hotspot_summary: 'fuzz.hotspot.summary',
	disposable_sandbox_boundary: 'fuzz.disposable.sandbox_boundary',
	destructive_permission: 'fuzz.disposable.destructive_permission',
	mutation_isolation: 'fuzz.mutation.isolation',
	mutation_isolation_artifact: 'fuzz.mutation.isolation',
	delete_boundary: 'fuzz.delete.boundary',
	delete_boundary_artifact: 'fuzz.delete.boundary',
	sandbox_isolation_proof: 'fuzz.disposable.sandbox_isolation_proof',
	teardown_discard: 'fuzz.disposable.teardown_discard',
	external_http_guardrail: 'fuzz.external_http.guardrail',
	runtime_access: 'fuzz.runtime.access',
	result_envelope: 'fuzz.result.envelope',
	normalized_fuzz_result: 'fuzz.result.normalized',
	coverage: 'fuzz.coverage',
};
const FUZZ_ARTIFACT_ROLES_BY_SEMANTIC_KEY = Object.fromEntries(
	Object.entries(FUZZ_ARTIFACT_SEMANTIC_KEYS).map(([role, semanticKey]) => [semanticKey, role])
);

const REQUIRED_WP_CODEBOX_FUZZ_CONTRACT_PATHS = [
	'abilities.wordpressRuntime.runFuzzSuite',
	'abilities.wordpressRuntime.runWorkload',
	'commands.wordpressRuntime.runFuzzSuite',
	'commands.wordpressRuntime.runWorkload',
	'schemas.wordpressRuntime.fuzzSuite',
	'schemas.wordpressRuntime.fuzzSuiteResult',
	'schemas.wordpressRuntime.workloadRun',
];

function wpCodeboxRuntimeContractManifest(options = {}) {
	const manifest = options.runtimeContractManifest || options.runtime_contract_manifest || options.manifest || options.contractManifest || options.contract_manifest;
	return objectOrUndefined(manifest) || DEFAULT_RUNTIME_CONTRACT_MANIFEST;
}

function wpCodeboxWordPressRuntimeContracts(options = {}) {
	const manifest = wpCodeboxRuntimeContractManifest(options);
	return {
		manifest,
		abilities: objectOrUndefined(manifest?.abilities?.wordpressRuntime) || {},
		commands: objectOrUndefined(manifest?.commands?.wordpressRuntime) || {},
		schemas: objectOrUndefined(manifest?.schemas?.wordpressRuntime) || {},
	};
}


function missingWpCodeboxFuzzRuntimeContractPaths(manifest) {
	if (!objectOrUndefined(manifest)) {
		return [...REQUIRED_WP_CODEBOX_FUZZ_CONTRACT_PATHS];
	}
	return REQUIRED_WP_CODEBOX_FUZZ_CONTRACT_PATHS.filter((pathName) => typeof valueAtPath(manifest, pathName) !== 'string' || valueAtPath(manifest, pathName).trim() === '');
}

function valueAtPath(value, pathName) {
	return pathName.split('.').reduce((current, part) => current?.[part], value);
}

function wpCodeboxFuzzSuiteAbility(options = {}) {
	return wpCodeboxWordPressRuntimeContracts(options).abilities.runFuzzSuite || DEFAULT_FUZZ_SUITE_ABILITY;
}

function wpCodeboxWordPressWorkloadRunAbility(options = {}) {
	return wpCodeboxWordPressRuntimeContracts(options).abilities.runWorkload || DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY;
}

function wpCodeboxFuzzSuiteSchema(options = {}) {
	return wpCodeboxWordPressRuntimeContracts(options).schemas.fuzzSuite || WP_CODEBOX_FUZZ_SUITE_SCHEMA;
}

function wpCodeboxFuzzSuiteResultSchema(options = {}) {
	return wpCodeboxWordPressRuntimeContracts(options).schemas.fuzzSuiteResult || WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA;
}

function wpCodeboxWordPressWorkloadRunSchema(options = {}) {
	return wpCodeboxWordPressRuntimeContracts(options).schemas.workloadRun || DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA;
}

function wpCodeboxFuzzSuiteCommand(options = {}) {
	return wpCodeboxWordPressRuntimeContracts(options).commands.runFuzzSuite;
}

function wpCodeboxWordPressWorkloadRunCommand(options = {}) {
	return wpCodeboxWordPressRuntimeContracts(options).commands.runWorkload;
}

function wpCodeboxFuzzSuiteInput(options = {}) {
	const source = normalizeHomeboyFuzzWorkloadSource(options);
	const executionRequest = options.executionRequest ?? options.execution_request;
	const context = {
		packageRoot: options.packageRoot || options.package_root || source?.packageRoot || source?.package_root || source?.metadata?.package_root || source?.metadata?.packageRoot || source?.metadata?.homeboy_runtime_context?.package_root,
		executionRequest,
	};
	const cases = normalizeWpCodeboxFuzzSuiteCases(source || options, context);
	const artifacts = source?.artifacts || options.artifacts;
	const postprocessBinding = options.postprocessBinding || options.postprocess_binding || source?.postprocess_binding || source?.postprocessBinding;
	const fixturePlan = normalizeFuzzFixturePlan(options.fixturePlan || options.fixture_plan || options.metadata?.fixture_plan || options.metadata?.fixturePlan || source?.fixture_plan || source?.fixturePlan || source?.metadata?.fixture_plan || source?.metadata?.fixturePlan);
	const restMutationOptIns = normalizeRestMutationOptIns(options.restMutationOptIns || options.rest_mutation_opt_ins || options.restMutationOptIn || options.rest_mutation_opt_in || options.metadata?.rest_mutation_opt_ins || options.metadata?.restMutationOptIns || source?.rest_mutation_opt_ins || source?.restMutationOptIns || source?.metadata?.rest_mutation_opt_ins || source?.metadata?.restMutationOptIns);
	return stripUndefined({
		schema: wpCodeboxFuzzSuiteSchema(options),
		id: options.id || options.runId || options.run_id,
		goal: options.goal || options.instructions,
		execution_request: executionRequest,
		version: options.version,
		target: options.target,
		cases,
		steps: normalizeWordPressWorkloadSteps(source?.steps || options.steps),
		metadata: stripUndefined({
			...(objectOrUndefined(options.metadata) || {}),
			disposableSandboxBoundary: suiteInputRequiresDisposableLifecycleArtifacts({ cases, metadata: options.metadata }) ? disposableSandboxBoundaryEvidence(options) : undefined,
			workload: objectOrUndefined(options.workload),
			seeds: normalizeArray(options.seeds).length > 0 ? normalizeArray(options.seeds) : undefined,
			fixture_plan: fixturePlan,
			rest_mutation_opt_ins: restMutationOptIns,
			limits: objectOrUndefined(options.limits),
			coverage: objectOrUndefined(options.coverage),
			runtime_profile: objectOrUndefined(options.runtimeProfile || options.runtime_profile),
			artifacts: objectOrUndefined(artifacts),
			postprocess_binding: objectOrUndefined(postprocessBinding),
		}),
	});
}

function normalizeFuzzFixturePlan(value) {
	const plan = objectOrUndefined(value);
	if (!plan) {
		return undefined;
	}
	const refs = normalizeManifestRefs(plan.refs || plan.ref || plan.artifact_refs || plan.artifactRefs);
	const data = objectOrUndefined(plan.data || plan.fixtures || plan.fixture_data || plan.fixtureData);
	return stripUndefined({
		schema: plan.schema || 'homeboy/wordpress-fuzz-fixture-plan/v1',
		id: plan.id || plan.plan_id || plan.planId,
		refs: refs.length > 0 ? refs : undefined,
		data,
		metadata: objectOrUndefined(plan.metadata),
	});
}

function disposableSandboxBoundaryEvidence(options = {}) {
	const explicit = objectOrUndefined(options.disposableSandboxBoundary || options.disposable_sandbox_boundary || options.metadata?.disposableSandboxBoundary || options.metadata?.disposable_sandbox_boundary) || {};
	return stripUndefined({
		disposable: true,
		destructivePermission: true,
		teardown: explicit.teardown === 'destroy' ? 'destroy' : 'discard',
		backend: explicit.backend || 'wordpress-playground',
		environment: explicit.environment || 'wordpress',
		hostAccess: explicit.hostAccess || explicit.host_access || 'declared-mounts-only',
		metadata: objectOrUndefined(explicit.metadata),
	});
}

function normalizeRestMutationOptIns(value) {
	const manifest = Array.isArray(value) ? value : objectOrUndefined(value);
	if (!manifest) {
		return undefined;
	}
	const manifestObject = Array.isArray(manifest) ? {} : manifest;
	const refs = normalizeManifestRefs(manifestObject.refs || manifestObject.ref || manifestObject.artifact_refs || manifestObject.artifactRefs);
	const entries = (Array.isArray(manifest) ? manifest : normalizeArray(manifest.entries || manifest.opt_ins || manifest.optIns || manifest.cases || manifest.routes))
		.map(normalizeRestMutationOptInEntry)
		.filter(Boolean);
	return stripUndefined({
		schema: manifestObject.schema || 'homeboy/wordpress-rest-mutation-opt-ins/v1',
		id: manifestObject.id || manifestObject.manifest_id || manifestObject.manifestId,
		refs: refs.length > 0 ? refs : undefined,
		entries: entries.length > 0 ? entries : undefined,
		data: objectOrUndefined(manifestObject.data),
		metadata: objectOrUndefined(manifestObject.metadata),
	});
}

function normalizeRestMutationOptInEntry(entry = {}) {
	const source = objectOrUndefined(entry);
	if (!source) {
		return undefined;
	}
	return stripUndefined({
		id: source.id || source.opt_in_id || source.optInId,
		surface_id: source.surface_id || source.surfaceId,
		route: source.route || source.path,
		method: source.method ? String(source.method).toUpperCase() : undefined,
		allowed: source.allowed !== false,
		fixture_ref: source.fixture_ref || source.fixtureRef,
		contract_ref: source.contract_ref || source.contractRef,
		metadata: objectOrUndefined(source.metadata),
	});
}

function normalizeManifestRefs(value) {
	let refs = [];
	if (Array.isArray(value)) {
		refs = value;
	} else if (value !== undefined && value !== null) {
		refs = [value];
	}
	return refs.map((entry) => {
		if (typeof entry === 'string') {
			return { ref: entry };
		}
		const source = objectOrUndefined(entry);
		return source ? stripUndefined({ id: source.id, ref: source.ref || source.url || source.path, path: source.path, pointer: source.pointer, semantic_key: source.semantic_key || source.semanticKey }) : undefined;
	}).filter(Boolean);
}

function wordpressFuzzPostprocessBinding(options = {}) {
	const required = options.required === false ? false : true;
	return {
		...WORDPRESS_FUZZ_POSTPROCESS_BINDING,
		outputs: WORDPRESS_FUZZ_POSTPROCESS_OUTPUTS.map((artifact) => ({ ...artifact, required })),
		artifact_outputs: WORDPRESS_FUZZ_POSTPROCESS_OUTPUTS.map((artifact) => ({ ...artifact, required })),
		proof_outputs: WORDPRESS_FUZZ_POSTPROCESS_BINDING.proof_outputs.map((artifact) => ({ ...artifact, required })),
	};
}

function wordpressFuzzPostprocessArtifactDeclarations(options = {}) {
	const required = options.required === false ? false : true;
	const bySemanticKey = new Map(DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS.map((artifact) => [artifact.semantic_key, artifact]));
	for (const artifact of WORDPRESS_FUZZ_POSTPROCESS_OUTPUTS) {
		bySemanticKey.set(artifact.semantic_key, { ...(bySemanticKey.get(artifact.semantic_key) || {}), ...artifact, required });
	}
	return [...bySemanticKey.values()];
}

function wordpressFuzzPostprocessExpectedArtifacts() {
	return [...new Set([...DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS, ...WORDPRESS_FUZZ_POSTPROCESS_OUTPUTS.map((artifact) => artifact.name)])];
}

function normalizeHomeboyFuzzWorkloadSource(options = {}) {
	if (options?.schema === HOMEBOY_FUZZ_WORKLOAD_SCHEMA) {
		return options;
	}
	const workload = options.workload;
	if (workload?.schema === HOMEBOY_FUZZ_WORKLOAD_SCHEMA) {
		return workload;
	}
	if (options.homeboyFuzzWorkload?.schema === HOMEBOY_FUZZ_WORKLOAD_SCHEMA || options.homeboy_fuzz_workload?.schema === HOMEBOY_FUZZ_WORKLOAD_SCHEMA) {
		return options.homeboyFuzzWorkload || options.homeboy_fuzz_workload;
	}
	return undefined;
}

function normalizeWpCodeboxFuzzSuiteCases(source = {}, context = {}) {
	const directCases = normalizeArray(source.cases);
	if (source.schema !== HOMEBOY_FUZZ_WORKLOAD_SCHEMA) {
		return directCases;
	}
	const planCases = homeboyFuzzWorkloadPlanCases(source);
	if (planCases.length > 0) {
		return planCases.map((entry, index) => homeboyFuzzWorkloadPlanCaseToWpCodeboxCase(entry, source, index, context));
	}
	if (directCases.length === 0) {
		const defaultCase = homeboyFuzzWorkloadDefaultCase(source);
		return defaultCase ? [homeboyFuzzWorkloadCaseToWpCodeboxCase(defaultCase, source, 0, context)] : [];
	}
	return directCases.map((entry, index) => homeboyFuzzWorkloadCaseToWpCodeboxCase(entry, source, index, context));
}

function homeboyFuzzWorkloadDefaultCase(manifest = {}) {
	const workload = objectOrUndefined(manifest.workload) || {};
	const workloadPath = homeboyFuzzManifestWorkloadPath(manifest);
	const workloadDefinition = objectOrUndefined(workload.definition || manifest.workload_definition || manifest.workloadDefinition || manifest.metadata?.workload_definition || manifest.metadata?.workloadDefinition);
	const genericCommand = homeboyFuzzWorkloadGenericPrimitiveCommand(manifest);
	if (!genericCommand && !workloadDefinition && (typeof workloadPath !== 'string' || workloadPath.trim() === '')) {
		return undefined;
	}
	const type = workload.type || manifest.workload_type || manifest.workloadType || manifest.metadata?.workload_type || manifest.metadata?.workloadType || typeFromWorkloadPath(workloadPath);
	const entry = workload.entry || manifest.entry || manifest.metadata?.entry || manifest.id;
	const activation = manifest.metadata?.fixture?.activation || firstHomeboyFuzzWorkloadActivation(manifest);
	return {
		case_id: `${manifest.id || 'fuzz-workload'}:default`,
		artifacts: normalizeArray(manifest.artifacts?.expected),
		intent: stripUndefined({
			schema: 'homeboy/fuzz-workload-intent/v1',
			type: 'wordpress-plugin-workload',
			plugin: activation ? { activation } : undefined,
			execute: stripUndefined({
				workload_ref: 'default',
				path: workloadPath,
				type,
				entry,
				definition: workloadDefinition,
			}),
			collect: normalizeArray(manifest.artifacts?.expected).map((artifact) => ({ artifact: artifact.name })).filter((item) => item.artifact),
		}),
	};
}

function homeboyFuzzManifestWorkloadPath(manifest = {}) {
	const workload = objectOrUndefined(manifest.workload) || {};
	return workload.path || manifest.workload_path || manifest.workloadPath || manifest.metadata?.workload_path || manifest.metadata?.workloadPath;
}

function typeFromWorkloadPath(value) {
	const filePath = typeof value === 'string' ? value.trim().toLowerCase() : '';
	if (filePath.endsWith('.php')) {
		return 'php';
	}
	if (filePath.endsWith('.json')) {
		return 'json';
	}
	return undefined;
}

function homeboyFuzzWorkloadPlanCases(manifest = {}) {
	return normalizeArray(manifest.plan?.targets).flatMap((target) => (
		normalizeArray(target?.cases).map((testCase) => ({
			...testCase,
			target_id: target.id,
			surface_id: target.surface_id || target.surfaceId,
			target_metadata: objectOrUndefined(target.metadata),
		}))
	));
}

function homeboyFuzzWorkloadPlanCaseToWpCodeboxCase(entry = {}, manifest = {}, index = 0) {
	const caseId = entry.case_id || entry.caseId || entry.id || `${manifest.id || 'fuzz-workload'}:${index}`;
	const artifacts = normalizeHomeboyFuzzCaseArtifacts(entry, manifest);
	const target = homeboyFuzzPlanCaseTarget(entry);
	const input = homeboyFuzzPlanCaseRuntimeInput(entry, manifest, caseId);
	const restMutationOptIn = objectOrUndefined(entry.metadata?.rest_mutation_opt_in || entry.metadata?.restMutationOptIn || entry.rest_mutation_opt_in || entry.restMutationOptIn);
	return stripUndefined({
		id: caseId,
		case_id: caseId,
		target,
		description: entry.description || manifest.label,
		input,
		inputs: objectOrUndefined(entry.inputs),
		phases: homeboyFuzzWorkloadPlanCasePhases(entry, manifest, artifacts),
		artifacts,
		metadata: stripUndefined({
			...(objectOrUndefined(entry.metadata) || {}),
			rest_mutation_opt_in: restMutationOptIn,
			source_schema: HOMEBOY_FUZZ_WORKLOAD_SCHEMA,
			source_manifest_id: manifest.id,
			source_plan_case: true,
			target_id: entry.target_id,
			surface_id: entry.surface_id,
			target_metadata: objectOrUndefined(entry.target_metadata),
		}),
	});
}

function homeboyFuzzWorkloadPlanCasePhases(entry = {}, manifest = {}, artifacts = []) {
	if (objectOrUndefined(entry.phases)) {
		return entry.phases;
	}
	if (homeboyFuzzPlanCaseTarget(entry).kind === 'runtime-action') {
		return undefined;
	}
	const activation = manifest.metadata?.fixture?.activation || firstHomeboyFuzzWorkloadActivation(manifest);
	const setup = typeof activation === 'string' && activation.trim() !== ''
		? [wpCodeboxPluginActivationStep(activation)]
		: undefined;
	const command = homeboyFuzzPlanCaseRuntimeCommand(entry);
	const runtimeInput = homeboyFuzzPlanCaseRuntimeInput(entry, manifest, entry.case_id || entry.caseId || entry.id);
	const action = typeof command === 'string' && command.trim() !== ''
		? [{ command, args: homeboyFuzzRuntimeCommandArgs(runtimeInput) }]
		: [];
	const assert = artifacts
		.map((artifact) => artifact.name)
		.filter(Boolean)
		.map((artifact) => ({ command: 'wordpress.collect-workload-result', args: [`artifact=${artifact}`] }));
	return stripUndefined({ setup, action, assert: assert.length > 0 ? assert : undefined });
}

function homeboyFuzzPlanCaseTarget(entry = {}) {
	if (entry.target?.kind === 'runtime-action') {
		return stripUndefined({
			kind: 'runtime-action',
			id: entry.target.id,
			entrypoint: entry.target.entrypoint,
			label: entry.target.label,
			metadata: objectOrUndefined(entry.target.metadata),
		});
	}
	const runtimeActionInput = homeboyFuzzPlanCaseRuntimeActionInput(entry);
	if (runtimeActionInput?.type) {
		return { kind: 'runtime-action', id: `runtime-action:${runtimeActionInput.type}`, entrypoint: runtimeActionInput.type };
	}
	const command = homeboyFuzzPlanCaseRuntimeCommand(entry);
	return { kind: 'runtime', id: command, entrypoint: command };
}

function homeboyFuzzRuntimeCommandArgs(input) {
	return Array.isArray(input?.args) ? input.args : homeboyFuzzCommandArgs(input);
}

function homeboyFuzzPlanCaseRuntimeCommand(entry = {}) {
	const runtimeOperation = objectOrUndefined(entry.runtime_operation || entry.runtimeOperation || entry.metadata?.runtime_operation || entry.metadata?.runtimeOperation);
	if (typeof runtimeOperation?.command === 'string' && runtimeOperation.command.trim() !== '') {
		return runtimeOperation.command.trim();
	}
	return entry.command || entry.target?.entrypoint || entry.target?.id || 'wordpress.run-fuzz-case';
}

function homeboyFuzzPlanCaseRuntimeInput(entry = {}, manifest = {}, caseId) {
	if (objectOrUndefined(entry.input) && typeof entry.input.type === 'string') {
		return homeboyFuzzRuntimeCommandInput(entry.input);
	}
	const runtimeActionInput = homeboyFuzzPlanCaseRuntimeActionInput(entry);
	if (runtimeActionInput) {
		return runtimeActionInput;
	}
	if (objectOrUndefined(entry.input)) {
		return homeboyFuzzRuntimeCommandInput(entry.input);
	}
	const restMutationOptIn = objectOrUndefined(entry.metadata?.rest_mutation_opt_in || entry.metadata?.restMutationOptIn || entry.rest_mutation_opt_in || entry.restMutationOptIn);
	const fixtureBinding = objectOrUndefined(entry.metadata?.fixture_binding || entry.metadata?.fixtureBinding || entry.fixture_binding || entry.fixtureBinding);
	return stripUndefined({
		case_id: caseId,
		target_id: entry.target_id,
		surface_id: entry.surface_id,
		intent: entry.intent,
		operation_id: entry.operation_id || entry.operationId,
		operation: objectOrUndefined(entry.operation),
		fixture_binding: fixtureBinding,
		rest_mutation_opt_in: restMutationOptIn,
		mutation_lifecycle: objectOrUndefined(entry.metadata?.mutation_lifecycle || entry.metadata?.mutationLifecycle || entry.mutation_lifecycle || entry.mutationLifecycle),
		runtime_operation: objectOrUndefined(entry.runtime_operation || entry.runtimeOperation || entry.metadata?.runtime_operation || entry.metadata?.runtimeOperation),
		seed: entry.seed || manifest.seed,
		skip_reasons: nonEmptyArray(entry.skip_reasons || entry.skipReasons),
		destructive_reasons: nonEmptyArray(entry.destructive_reasons || entry.destructiveReasons),
	});
}

function homeboyFuzzPlanCaseRuntimeActionInput(entry = {}) {
	const runtimeOperation = objectOrUndefined(entry.runtime_operation || entry.runtimeOperation || entry.metadata?.runtime_operation || entry.metadata?.runtimeOperation);
	if (!runtimeOperation || runtimeOperation.status !== 'ready') {
		return undefined;
	}
	const input = objectOrUndefined(runtimeOperation.input) || {};
	if (runtimeOperation.family === 'rest') {
		return stripUndefined({
			type: 'rest_request',
			method: input.method || entry.operation?.method,
			path: input.route || input.path || entry.operation?.route || entry.operation?.path,
			params: input.query_params || input.route_params,
			body_json: input.request_body,
		});
	}
	if (runtimeOperation.family === 'crud') {
		return stripUndefined({ ...(objectOrUndefined(entry.operation) || input), type: 'crud_operation' });
	}
	if (runtimeOperation.family === 'admin_page') {
		return stripUndefined({ type: 'admin_page', path: input.path || entry.operation?.path, wait_for: input.wait_for || input.waitFor });
	}
	if (runtimeOperation.family === 'frontend_page') {
		return stripUndefined({ type: 'page', path: input.path || entry.operation?.path, wait_for: input.wait_for || input.waitFor });
	}
	if (runtimeOperation.family === 'database') {
		return stripUndefined({
			type: 'php',
			code: entry.operation?.statement
				? `$wpdb->query( ${JSON.stringify(String(entry.operation.statement))} );`
				: `$wpdb->get_results( ${JSON.stringify(String(input.query || entry.operation?.query || 'SELECT 1'))} );`,
			diagnostics: { capture: ['wpdb-queries'] },
		});
	}
	return undefined;
}

function nonEmptyArray(value) {
	const normalized = normalizeArray(value);
	return normalized.length > 0 ? normalized : undefined;
}

function homeboyFuzzCommandArgs(input) {
	const object = objectOrUndefined(input);
	if (!object) {
		return [];
	}
	return Object.entries(object).flatMap(([key, value]) => {
		if (value === undefined || value === null) {
			return [];
		}
		if (Array.isArray(value)) {
			return [`${key}=${value.join(',')}`];
		}
		if (typeof value === 'object') {
			return [`${key}=${JSON.stringify(value)}`];
		}
		return [`${key}=${value}`];
	});
}

function firstHomeboyFuzzWorkloadActivation(manifest = {}) {
	for (const entry of normalizeArray(manifest.cases)) {
		const activation = entry?.intent?.plugin?.activation;
		if (typeof activation === 'string' && activation.trim() !== '') {
			return activation;
		}
	}
	return undefined;
}

function homeboyFuzzWorkloadCaseToWpCodeboxCase(entry = {}, manifest = {}, index = 0, context = {}) {
	const caseId = entry.case_id || entry.caseId || entry.id || `${manifest.id || 'fuzz-workload'}:${index}`;
	const intent = objectOrUndefined(entry.intent) || {};
	const execute = objectOrUndefined(intent.execute) || {};
	const artifacts = normalizeHomeboyFuzzCaseArtifacts(entry, manifest);
	const command = homeboyFuzzWorkloadGenericPrimitiveCommand(manifest) || 'wordpress.run-workload';
	const input = homeboyFuzzWorkloadRuntimeCommandInput(entry, manifest, execute, context);
	return stripUndefined({
		id: caseId,
		case_id: caseId,
		target: { kind: 'runtime', id: command, entrypoint: command },
		description: entry.description || manifest.label,
		input,
		inputs: objectOrUndefined(entry.inputs),
		phases: homeboyFuzzWorkloadCasePhases(entry, manifest, intent, artifacts, context),
		artifacts,
		metadata: stripUndefined({
			...(objectOrUndefined(entry.metadata) || {}),
			source_schema: HOMEBOY_FUZZ_WORKLOAD_SCHEMA,
			source_manifest_id: manifest.id,
			intent: objectOrUndefined(entry.intent),
		}),
	});
}

function homeboyFuzzRuntimeCommandInput(input) {
	const direct = objectOrUndefined(input);
	if (!direct) {
		return undefined;
	}
	if (Array.isArray(direct.args) || direct.operation || direct.runtime_operation || direct.fixture_binding || direct.rest_mutation_opt_in || direct.mutation_lifecycle) {
		return direct;
	}
	const args = homeboyFuzzCommandArgs(direct);
	return args.length > 0 ? { args } : direct;
}

function homeboyFuzzWorkloadRuntimeCommandInput(entry = {}, manifest = {}, execute = {}, context = {}) {
	if (objectOrUndefined(entry.input)) {
		return homeboyFuzzRuntimeCommandInput(entry.input);
	}
	const workloadDefinition = objectOrUndefined(execute.definition) || objectOrUndefined(manifest.workload?.definition);
	if (workloadDefinition) {
		return homeboyFuzzWorkloadRunInputFromDefinition(workloadDefinition, { entry: execute.entry || manifest.workload?.entry, executionRequest: context.executionRequest });
	}
	const workloadPath = resolveWorkloadPath(execute.path || homeboyFuzzManifestWorkloadPath(manifest), context);
	if (typeof workloadPath === 'string' && workloadPath.trim() !== '') {
		const workloadType = String(execute.type || manifest.workload?.type || typeFromWorkloadPath(workloadPath) || '').toLowerCase();
		if (workloadType === 'php') {
			return wpCodeboxWordPressWorkloadRunInput({
				id: execute.entry || manifest.workload?.entry,
				executionRequest: context.executionRequest,
				steps: [{ command: 'wordpress.run-workload', args: [`path=${workloadPath}`, 'type=php'] }],
				metadata: stripUndefined({
					source_path: workloadPath,
					source_entry: execute.entry || manifest.workload?.entry,
					source_type: 'php',
				}),
			});
		}
		const workloadInput = homeboyFuzzWorkloadRunInputFromFile(workloadPath, { entry: execute.entry || manifest.workload?.entry, executionRequest: context.executionRequest });
		if (workloadInput) {
			return workloadInput;
		}
		if (workloadType === 'json' || workloadPath.toLowerCase().endsWith('.json')) {
			throw new Error(`Unable to hydrate JSON WordPress workload: ${workloadPath}`);
		}
		return { args: [`path=${workloadPath}`] };
	}
	const parameters = objectOrUndefined(execute.parameters);
	if (parameters) {
		return homeboyFuzzRuntimeCommandInput(parameters);
	}
	return undefined;
}

function homeboyFuzzWorkloadRunInputFromDefinition(source = {}, options = {}) {
	const steps = normalizeWordPressWorkloadSteps(source.run || source.steps);
	if (steps.length === 0) {
		return undefined;
	}
	return wpCodeboxWordPressWorkloadRunInput({
		id: source.id || options.entry,
		executionRequest: options.executionRequest,
		before: source.before,
		steps,
		after: source.after,
		artifacts: source.artifacts,
		metadata: stripUndefined({
			...(objectOrUndefined(source.metadata) || {}),
			source: 'inline',
			source_entry: options.entry,
		}),
	});
}

function homeboyFuzzWorkloadRunInputFromFile(workloadPath, options = {}) {
	const filePath = String(workloadPath || '').trim();
	if (!filePath || !fs.existsSync(filePath)) {
		return undefined;
	}
	let source;
	try {
		source = JSON.parse(fs.readFileSync(filePath, 'utf8'));
	} catch {
		return undefined;
	}
	const packageRoot = packageRootFromWorkloadPath(filePath);
	source = expandWorkloadTemplateTokens(source, { packageRoot, sourceFilePath: filePath });
	const steps = normalizeWordPressWorkloadSteps(source.run || source.steps, { packageRoot, sourceFilePath: filePath });
	if (steps.length === 0) {
		return undefined;
	}
	return wpCodeboxWordPressWorkloadRunInput({
		id: source.id || options.entry,
		executionRequest: options.executionRequest,
		before: source.before,
		steps,
		after: source.after,
		artifacts: source.artifacts,
		metadata: stripUndefined({
			...(objectOrUndefined(source.metadata) || {}),
			source_path: filePath,
			source_entry: options.entry,
		}),
	});
}

function packageRootFromWorkloadPath(filePath) {
	const parent = path.basename(path.dirname(filePath));
	if (['bench', 'fuzz', 'manifests', 'tools'].includes(parent)) {
		return path.dirname(path.dirname(filePath));
	}
	return path.dirname(filePath);
}

function expandWorkloadTemplateTokens(value, replacements = {}) {
	if (typeof value === 'string') {
		return value
			.replaceAll('${package.root}', replacements.packageRoot || '')
			.replaceAll('${source.file}', replacements.sourceFilePath || '');
	}
	if (Array.isArray(value)) {
		return value.map((item) => expandWorkloadTemplateTokens(item, replacements));
	}
	if (objectOrUndefined(value)) {
		return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, expandWorkloadTemplateTokens(item, replacements)]));
	}
	return value;
}

function homeboyFuzzWorkloadCasePhases(entry = {}, manifest = {}, intent = {}, artifacts = [], context = {}) {
	if (objectOrUndefined(entry.phases)) {
		return entry.phases;
	}
	const execute = objectOrUndefined(intent.execute) || {};
	const activation = intent.plugin?.activation;
	const workloadPath = resolveWorkloadPath(execute.path || homeboyFuzzManifestWorkloadPath(manifest), context);
	const workloadDefinition = objectOrUndefined(execute.definition) || objectOrUndefined(manifest.workload?.definition);
	const genericCommand = homeboyFuzzWorkloadGenericPrimitiveCommand(manifest);
	const setup = typeof activation === 'string' && activation.trim() !== ''
		? [wpCodeboxPluginActivationStep(activation)]
		: undefined;
	const action = homeboyFuzzWorkloadCaseAction({ genericCommand, workloadPath, workloadDefinition, execute, pluginSlug: pluginSlugFromActivation(activation), executionRequest: context.executionRequest });
	const collect = normalizeArray(intent.collect).length > 0 ? normalizeArray(intent.collect) : artifacts.map((artifact) => ({ artifact: artifact.name }));
	const assert = collect
		.map((item) => item?.artifact)
		.filter(Boolean)
		.map((artifact) => ({ command: 'wordpress.collect-workload-result', args: [`artifact=${artifact}`] }));
	return stripUndefined({ setup, action, assert: assert.length > 0 ? assert : undefined });
}

function wpCodeboxPluginActivationStep(plugin) {
	return { command: 'wordpress.plugin-state', args: ['action=activate', `plugin=${plugin}`] };
}

function homeboyFuzzWorkloadCaseAction({ genericCommand, workloadPath, workloadDefinition, execute = {}, pluginSlug, executionRequest } = {}) {
	if (typeof genericCommand === 'string') {
		return [{ command: genericCommand, args: homeboyFuzzCommandArgs(objectOrUndefined(execute.parameters) || {}) }];
	}
	if (typeof workloadPath === 'string' && workloadPath.trim() !== '') {
		const workloadType = String(execute.type || typeFromWorkloadPath(workloadPath) || '').toLowerCase();
		const jsonWorkload = workloadType === 'json' || workloadPath.toLowerCase().endsWith('.json') ? homeboyFuzzWorkloadRunInputFromFile(workloadPath, { entry: execute.entry, executionRequest }) : undefined;
		if (jsonWorkload) {
			if (pluginSlug) {
				jsonWorkload.metadata = { ...(objectOrUndefined(jsonWorkload.metadata) || {}), plugin_slug: pluginSlug };
			}
			return [{ command: 'wordpress.run-workload', args: [`workload-json=${JSON.stringify(jsonWorkload)}`] }];
		}
		if (workloadType === 'json' || workloadPath.toLowerCase().endsWith('.json')) {
			throw new Error(`Unable to hydrate JSON WordPress workload: ${workloadPath}`);
		}
		return [{ command: 'wordpress.run-workload', args: [`path=${workloadPath}`, ...(workloadPath.toLowerCase().endsWith('.php') ? ['type=php'] : [])] }];
	}
	if (objectOrUndefined(workloadDefinition)) {
		return [{ command: 'wordpress.run-workload' }];
	}
	return [];
}

function pluginSlugFromActivation(activation) {
	return typeof activation === 'string' && activation.includes('/') ? activation.split('/')[0].trim() : undefined;
}

function resolveWorkloadPath(value, context = {}) {
	if (typeof value !== 'string' || value.trim() === '') {
		return value;
	}
	const packageRoot = context.packageRoot || context.package_root;
	return packageRoot ? value.replaceAll('${package.root}', packageRoot) : value;
}

function homeboyFuzzWorkloadGenericPrimitiveCommand(manifest = {}) {
	const command = manifest.metadata?.generic_primitive?.command || manifest.metadata?.genericPrimitive?.command;
	return typeof command === 'string' && command.trim() !== '' ? command.trim() : undefined;
}

function normalizeHomeboyFuzzCaseArtifacts(entry = {}, manifest = {}) {
	const byName = new Map();
	for (const artifact of [...normalizeArray(manifest.artifacts?.expected), ...normalizeArray(entry.artifacts)]) {
		if (!objectOrUndefined(artifact) || typeof artifact.name !== 'string' || artifact.name.trim() === '') {
			continue;
		}
		const existing = byName.get(artifact.name) || {};
		const existingMetadata = objectOrUndefined(existing.metadata) || {};
		byName.set(artifact.name, stripUndefined({
			...existing,
			name: artifact.name,
			path: artifact.path || artifact.relativePath || artifact.relative_path || existing.path,
			role: artifact.role || existing.role,
			kind: artifact.kind || existing.kind,
			contentType: artifact.contentType || artifact.content_type || existing.contentType,
			required: artifact.required !== false,
			metadata: stripUndefined({
				...existingMetadata,
				...(objectOrUndefined(artifact.metadata) || {}),
				semantic_key: artifact.semantic_key || artifact.semanticKey || existingMetadata.semantic_key,
				schema: artifact.schema || existingMetadata.schema,
			}),
		}));
	}
	return [...byName.values()];
}

function wpCodeboxFuzzSuiteTaskRequest(options = {}) {
	const input = wpCodeboxFuzzSuiteInput(options.input || options.abilityInput || options.ability_input || options);
	return wordpressRuntimeTaskRequest({
		...options,
		backend: options.backend || 'wp-codebox',
		runtime: options.runtime || options.runtimeId || options.runtime_id || 'wp-codebox',
		taskId: requiredString(options.taskId || options.task_id, 'taskId'),
		ability: options.ability || wpCodeboxFuzzSuiteAbility(options),
		abilityInput: input,
		artifactDeclarations: options.artifactDeclarations || options.artifact_declarations || wpCodeboxFuzzArtifactDeclarationsForInput(input),
		expectedArtifacts: options.expectedArtifacts || options.expected_artifacts || wpCodeboxFuzzExpectedArtifactsForInput(input),
		goal: options.goal || options.instructions || 'Delegate WordPress fuzz execution to WP Codebox and return the declared fuzz artifacts.',
		instructions: options.instructions || 'Delegate WordPress fuzz execution to WP Codebox and return the declared fuzz artifacts.',
	});
}

function wpCodeboxFuzzArtifactDeclarationsForInput(input = {}) {
	if (!suiteInputRequiresDisposableLifecycleArtifacts(input)) {
		return DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS;
	}
	return DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS.map((artifact) => (
		['sandbox-isolation-proof', 'mutation-isolation-artifact', 'delete-boundary-artifact'].includes(artifact.name) ? { ...artifact, required: true } : artifact
	));
}

function wpCodeboxFuzzExpectedArtifactsForInput(input = {}) {
	if (!suiteInputRequiresDisposableLifecycleArtifacts(input)) {
		return DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS;
	}
	return [...new Set([...DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS, 'sandbox-isolation-proof', 'mutation-isolation-artifact', 'delete-boundary-artifact'])];
}

function suiteInputRequiresDisposableLifecycleArtifacts(input = {}) {
	const metadata = objectOrUndefined(input.metadata) || {};
	return metadata.aggressive === true
		|| String(metadata.mode || metadata.fuzz_mode || metadata.fuzzMode || '').toLowerCase() === 'aggressive'
		|| normalizeArray(input.cases).some((testCase) => fuzzCaseRequiresDisposableLifecycle(testCase, testCase));
}

function wpCodeboxFuzzRuntimeTaskRequest(options = {}) {
	const providerRequest = wpCodeboxFuzzSuiteTaskRequest(options);
	return buildWordPressFuzzRuntimeTaskRequest({
		...options,
		provider: { id: 'wp-codebox', name: 'WP Codebox' },
		providerRequest,
		input: providerRequest.executor?.config?.runtime_task?.input,
		requirements: providerRequest.executor?.config?.runtime_requirements,
		artifactDeclarations: providerRequest.artifact_declarations,
		expectedArtifacts: providerRequest.expected_artifacts,
		instructions: providerRequest.instructions,
		providerMetadata: {
			wp_codebox: stripUndefined({
				ability: providerRequest.executor?.config?.runtime_task?.ability,
				runtime: providerRequest.executor?.runtime,
			}),
		},
	});
}

function wpCodeboxFuzzExecutionRequest(options = {}) {
	const input = wpCodeboxFuzzSuiteInput(options.input || options.abilityInput || options.ability_input || options);
	const taskId = requiredString(options.taskId || options.task_id || input.id, 'taskId');
	const ability = options.ability || wpCodeboxFuzzSuiteAbility(options);
	const command = wpCodeboxCommandFromFuzzAbility(ability, options);
	const runtimeRequirements = options.runtimeRequirements || options.runtime_requirements;
	return stripUndefined({
		schema: WP_CODEBOX_FUZZ_EXECUTION_SCHEMA,
		task_id: taskId,
		ability,
		command,
		input,
		runtime_requirements: objectOrUndefined(runtimeRequirements),
		artifact_declarations: options.artifactDeclarations || options.artifact_declarations || DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS,
		expected_artifacts: options.expectedArtifacts || options.expected_artifacts || DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS,
		instructions: options.instructions || 'Delegate WordPress fuzz execution to WP Codebox and return the declared fuzz artifacts.',
		metadata: stripUndefined({
			...(objectOrUndefined(options.metadata) || {}),
			executor: 'wp-codebox-direct-fuzz',
			runtime: options.runtime || options.runtimeId || options.runtime_id || 'wp-codebox',
		}),
	});
}

function wpCodeboxWordPressWorkloadRunInput(options = {}) {
	const metadata = objectOrUndefined(options.metadata);
	return stageWordPressRunWorkloadPhpFiles(stripUndefined({
		schema: wpCodeboxWordPressWorkloadRunSchema(options),
		id: options.id || options.runId || options.run_id,
		execution_request: options.executionRequest ?? options.execution_request,
		wordpress_version: options.wordpressVersion || options.wordpress_version,
		blueprint: options.blueprint,
		preview: objectOrUndefined(options.preview),
		mounts: normalizeArray(options.mounts),
		runtime_stack_mounts: normalizeArray(options.runtimeStackMounts || options.runtime_stack_mounts),
		runtime_overlays: normalizeArray(options.runtimeOverlays || options.runtime_overlays),
		runtime_env: objectOrUndefined(options.runtimeEnv || options.runtime_env),
		secret_env: normalizeArray(options.secretEnv || options.secret_env),
		staged_files: normalizeArray(options.stagedFiles || options.staged_files),
		before: normalizeArray(options.before),
		steps: normalizeWordPressWorkloadSteps(options.steps, options),
		after: normalizeArray(options.after),
		artifacts: normalizeArray(options.artifacts),
		metadata,
	}), { packageRoot: options.packageRoot || options.package_root, sourcePath: metadata?.source_path || metadata?.sourcePath });
}

function stageWordPressRunWorkloadPhpFiles(workload = {}, options = {}) {
	const originalStagedFileCount = normalizeArray(workload.staged_files || workload.stagedFiles).length;
	const stagedFiles = [...normalizeArray(workload.staged_files || workload.stagedFiles)];
	const packageRoot = options.packageRoot || (typeof options.sourcePath === 'string' ? packageRootFromWorkloadPath(options.sourcePath) : undefined);
	let changed = false;
	const phases = Object.fromEntries(['before', 'steps', 'after'].map((phase) => [phase, normalizeArray(workload[phase]).map((step) => {
		const stagedStep = stageWordPressRunWorkloadPhpStep(step, { stagedFiles, packageRoot });
		if (stagedStep !== step) {
			changed = true;
		}
		return stagedStep;
	})]));
	const normalizedWorkload = { ...workload };
	delete normalizedWorkload.stagedFiles;
	return changed || stagedFiles.length !== originalStagedFileCount ? { ...normalizedWorkload, ...phases, staged_files: stagedFiles } : workload;
}

function stageWordPressRunWorkloadPhpStep(step = {}, { stagedFiles = [], packageRoot } = {}) {
	if (step?.command !== 'wordpress.run-workload' || !Array.isArray(step.args)) {
		return step;
	}
	const pathArg = step.args.find((arg) => typeof arg === 'string' && arg.startsWith('path='));
	if (!pathArg) {
		return step;
	}
	const source = pathArg.slice('path='.length);
	const type = step.args.find((arg) => typeof arg === 'string' && arg.startsWith('type='))?.slice('type='.length).toLowerCase();
	if (!isLocalAbsolutePath(source) || !fs.existsSync(source) || (type && type !== 'php') || (!type && !source.toLowerCase().endsWith('.php'))) {
		return step;
	}
	const target = wpCodeboxStagedWorkloadFileTarget(source, packageRoot);
	if (!stagedFiles.some((entry) => objectOrUndefined(entry)?.source === source && objectOrUndefined(entry)?.target === target)) {
		stagedFiles.push({ source, target });
	}
	return { ...step, args: step.args.map((arg) => arg === pathArg ? `path=${target}` : arg) };
}

function wpCodeboxStagedWorkloadFileTarget(source, packageRoot) {
	let relative = path.basename(source);
	if (typeof packageRoot === 'string' && packageRoot.trim() && path.isAbsolute(packageRoot)) {
		const candidate = path.relative(packageRoot, source).replaceAll(path.sep, '/');
		if (candidate && !candidate.startsWith('..') && !path.isAbsolute(candidate)) {
			relative = candidate;
		}
	}
	return `/tmp/homeboy-wp-codebox-workloads/${relative}`;
}

function normalizeWordPressWorkloadSteps(steps, options = {}) {
	return normalizeArray(steps).map((step) => normalizeWordPressWorkloadStep(step, options)).filter(Boolean);
}

function normalizeWordPressWorkloadStep(step, options = {}) {
	if (!objectOrUndefined(step)) {
		return undefined;
	}
	const embeddedStep = embedSourcePhpWorkloadStep(step, options);
	if (embeddedStep) {
		return embeddedStep;
	}
	if (!isArtifactPostprocessCommand(step.command || step.type || step.name)) {
		return step;
	}
	const args = objectOrUndefined(step.args) || step;
	const input = objectOrUndefined(args.input) || {};
	const output = objectOrUndefined(args.output) || {};
	const parameters = objectOrUndefined(args.parameters) || {};
	const inputArtifactRoot = input.path || args.inputArtifactRoot || args.input_artifact_root;
	const outputArtifactPath = output.path || args.outputArtifactPath || args.output_artifact_path;
	const helperPath = packageRelativePath(args.helper || args.helperPath || args.helper_path, options.packageRoot);
	return stripUndefined({
		type: 'artifact-postprocess',
		action: args.action,
		helperPath,
		inputArtifactRoot,
		outputArtifactPath,
		maxInputBytes: input.max_bytes || input.maxBytes || args.maxInputBytes || args.max_input_bytes,
		maxArtifacts: input.max_artifacts || input.maxArtifacts || args.maxArtifacts || args.max_artifacts,
		expectedOutputSchema: output.schema || args.expectedOutputSchema || args.expected_output_schema,
		artifactName: output.artifact || args.artifactName || args.artifact_name,
		artifactKind: output.kind || args.artifactKind || args.artifact_kind,
		semantic: output.semantic_key || output.semantic || args.semantic,
		args: Array.isArray(step.args) ? step.args : [args.action, '${inputArtifactRoot}', '${outputArtifactPath}', JSON.stringify(parameters)],
		metadata: stripUndefined({
			...(objectOrUndefined(step.metadata) || {}),
			adapter: 'homeboy-extensions',
			contract: ARTIFACT_POSTPROCESS_CONTRACT,
		}),
	});
}

function packageRelativePath(value, packageRoot) {
	const requested = typeof value === 'string' ? value.trim() : '';
	if (!requested || !packageRoot || !path.isAbsolute(requested)) {
		return requested || undefined;
	}
	const relative = path.relative(packageRoot, requested).replaceAll(path.sep, '/');
	if (!relative || relative.startsWith('..') || path.isAbsolute(relative)) {
		return requested;
	}
	return relative;
}

function embedSourcePhpWorkloadStep(step, options = {}) {
	if (step.type !== 'php' || typeof step.file !== 'string' || step.file.trim() === '' || typeof step.code === 'string') {
		return undefined;
	}
	const sourceFile = resolveSourceWorkloadStepFile(step.file, options.sourceFilePath);
	if (!sourceFile) {
		return undefined;
	}
	const source = fs.readFileSync(sourceFile, 'utf8');
	return stripUndefined({
		...step,
		file: undefined,
		code: phpCallableSourceWrapper(source),
		metadata: stripUndefined({
			...(objectOrUndefined(step.metadata) || {}),
			source_file: sourceFile,
			embedded_source_file: true,
		}),
	});
}

function resolveSourceWorkloadStepFile(file, sourceFilePath) {
	const requested = String(file || '').trim();
	if (!requested || path.isAbsolute(requested) || requested.split(/[\\/]+/).includes('..')) {
		return undefined;
	}
	const sourcePath = typeof sourceFilePath === 'string' && path.isAbsolute(sourceFilePath) ? sourceFilePath : undefined;
	if (!sourcePath) {
		return undefined;
	}
	const sourceDirectory = path.dirname(sourcePath);
	const candidates = [
		path.resolve(sourceDirectory, requested),
		path.resolve(path.dirname(sourceDirectory), requested),
	];
	for (const candidate of candidates) {
		if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
			return candidate;
		}
	}
	return undefined;
}

function phpCallableSourceWrapper(source) {
	const body = String(source || '')
		.replace(/^\uFEFF/, '')
		.replace(/^\s*<\?php\s*/, '')
		.replace(/\?>\s*$/, '')
		.trim();
	return `$wp_codebox_embedded_callable = (function () {\n${body}\n})(); return is_callable($wp_codebox_embedded_callable) ? $wp_codebox_embedded_callable() : $wp_codebox_embedded_callable;`;
}

function isArtifactPostprocessCommand(value) {
	return ARTIFACT_POSTPROCESS_COMMAND_ALIASES.has(String(value || '').trim());
}

async function runWpCodeboxFuzzSuite(options = {}) {
	const runtimeRequest = wpCodeboxFuzzRuntimeTaskRequest(options);
	const request = wpCodeboxFuzzExecutionRequest(options);
	const runner = options.runFuzzSuite || options.runRuntimeTask || options.runTask;
	if (typeof runner === 'function') {
		const result = await runner(request, options);
		return normalizeWpCodeboxFuzzSuiteResult(result, { request, runtimeRequest });
	}

	const result = await runWpCodeboxPublicFuzzOperation({ ...options, request });
	return normalizeWpCodeboxFuzzSuiteResult(result, { request, runtimeRequest });
}

async function runWpCodeboxPublicFuzzOperation(options = {}) {
	const request = options.request || wpCodeboxFuzzExecutionRequest(options);
	const preflight = preflightWpCodeboxFuzzCapabilityContract({ ...options, request });
	const capabilities = preflight.capabilities;
	if (!preflight.ok) {
		return unsupportedWpCodeboxPublicFuzzResult({ request, capabilities, preflight });
	}
	const command = publicFuzzCliCommandForRequest(request, capabilities);
	if (!command) {
		return unsupportedWpCodeboxPublicFuzzResult({ request, capabilities, preflight });
	}

	const runnerMode = publicFuzzCliRunnerModeForRequest(request, { ...options, requiredPlanContracts: preflight.required });
	const cliResult = await runWpCodeboxPublicCli(command, wpCodeboxPublicCliInput(request, options), { ...options, runnerMode });
	if (cliResult.status !== 0) {
		return {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			request_id: request.task_id,
			status: 'failed',
			diagnostics: [{
				severity: 'error',
				code: 'wp_codebox_public_cli_execution_failed',
				message: `${wpCodeboxPublicCliBin(options)} codebox ${command} exited with status ${cliResult.status}.`,
				stderr: cliResult.stderr || undefined,
				stdout: cliResult.stdout || undefined,
			}],
		};
	}

	return parseWpCodeboxPublicCliJson(cliResult.stdout, { request, command });
}

function wpCodeboxPublicCliInput(request = {}, options = {}) {
	const runtimeRequirements = objectOrUndefined(wpCodeboxFuzzRequestRuntimeRequirements(request));
	validateWpCodeboxRuntimeRequirementMounts(runtimeRequirements, options);
	const input = stageArtifactPostprocessHelpers(wpCodeboxFuzzRequestInput(request, options), runtimeRequirements, options);
	return {
		...input,
		metadata: {
			...(objectOrUndefined(input.metadata) || {}),
			runtime_requirements: runtimeRequirements,
			homeboy_wp_codebox_fuzz_execution: request,
		},
	};
}

function validateWpCodeboxRuntimeRequirementMounts(runtimeRequirements = {}, options = {}) {
	if (options.validateRuntimeMounts === false || options.validate_runtime_mounts === false) {
		return;
	}
	for (const [collection, entries] of Object.entries({
		runtime_mounts: runtimeRequirements.runtime_mounts,
		extra_plugins: runtimeRequirements.extra_plugins,
	})) {
		for (const [index, entry] of normalizeArray(entries).entries()) {
			const requirement = objectOrUndefined(entry);
			if (!requirement) {
				continue;
			}
			const source = requirement.source;
			if (!isLocalAbsolutePath(source) || fs.existsSync(source)) {
				continue;
			}
			throw new Error(`WP Codebox runtime requirement ${collection}[${index}] source does not exist: ${source}`);
		}
	}
}

function isLocalAbsolutePath(value) {
	return typeof value === 'string' && path.isAbsolute(value) && !value.includes('${') && !value.includes('{{');
}

function stageArtifactPostprocessHelpers(input = {}, runtimeRequirements = {}, options = {}) {
	const pluginSlug = wpCodeboxRuntimePluginSlug(runtimeRequirements);
	const runtimeArtifactRoot = '/tmp/wp-codebox-artifacts';
	if (!pluginSlug || !Array.isArray(input.cases)) {
		return input;
	}
	const artifactRoot = wpCodeboxArtifactPostprocessRoot(options);
	let changed = false;
	const cases = input.cases.map((fuzzCase) => {
		const workload = objectOrUndefined(fuzzCase?.input);
		if (!workload || workload.schema !== DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA) {
			return fuzzCase;
		}
		const sourcePath = workload.metadata?.source_path || workload.metadata?.sourcePath;
		const packageRoot = typeof sourcePath === 'string' ? packageRootFromWorkloadPath(sourcePath) : undefined;
		const stagedFiles = [...normalizeArray(workload.staged_files || workload.stagedFiles)];
		const mounts = [...normalizeArray(workload.mounts)];
		for (const phase of ['before', 'steps', 'after']) {
			for (const step of normalizeArray(workload[phase])) {
				if ((step?.inputArtifactRoot === '${artifacts.root}' || step?.inputArtifactRoot === '{{artifacts.root}}') && artifactRoot) {
					step.inputArtifactRoot = runtimeArtifactRoot;
					if (!mounts.some((entry) => objectOrUndefined(entry)?.source === artifactRoot && objectOrUndefined(entry)?.target === runtimeArtifactRoot)) {
						mounts.push({ source: artifactRoot, target: runtimeArtifactRoot, mode: 'readwrite' });
						changed = true;
					}
				}
				const helperPath = typeof step?.helperPath === 'string' ? step.helperPath : undefined;
				if (!helperPath || !packageRoot || path.isAbsolute(helperPath) || helperPath.split(/[\\/]+/).includes('..')) {
					continue;
				}
				const source = path.join(packageRoot, helperPath);
				if (!fs.existsSync(source)) {
					continue;
				}
				const target = `/wordpress/wp-content/plugins/${pluginSlug}/${helperPath}`;
				if (!stagedFiles.some((entry) => objectOrUndefined(entry)?.source === source && objectOrUndefined(entry)?.target === target)) {
					stagedFiles.push({ source, target });
					changed = true;
				}
			}
		}
		if (stagedFiles.length === normalizeArray(workload.staged_files || workload.stagedFiles).length && mounts.length === normalizeArray(workload.mounts).length) {
			return fuzzCase;
		}
		changed = true;
		const normalizedWorkload = { ...workload };
		delete normalizedWorkload.stagedFiles;
		return { ...fuzzCase, input: { ...normalizedWorkload, mounts, staged_files: stagedFiles } };
	});
	return changed ? { ...input, cases } : input;
}

function wpCodeboxArtifactPostprocessRoot(options = {}) {
	const env = { ...process.env, ...(options.env || {}) };
	const resultsFile = options.resultsFile || options.results_file || env.resultsFile || env.HOMEBOY_FUZZ_RESULTS_FILE;
	const candidates = [
		options.artifactsRoot,
		options.artifactRoot,
		options.artifacts_root,
		options.artifact_root,
		env.HOMEBOY_ARTIFACT_ROOT,
		env.HOMEBOY_ARTIFACT_DIR,
		env.HOMEBOY_ARTIFACTS_DIR,
		env.HOMEBOY_RUN_ARTIFACT_ROOT,
		env.HOMEBOY_RUN_ARTIFACT_DIR,
		typeof resultsFile === 'string' && resultsFile.trim() ? path.dirname(resultsFile) : undefined,
	];
	for (const candidate of candidates) {
		if (typeof candidate === 'string' && candidate.trim() && fs.existsSync(candidate)) {
			return path.resolve(candidate);
		}
	}
	return undefined;
}

function wpCodeboxRuntimePluginSlug(runtimeRequirements = {}) {
	for (const plugin of normalizeArray(runtimeRequirements.extra_plugins)) {
		const slug = objectOrUndefined(plugin)?.slug;
		if (typeof slug === 'string' && slug.trim()) {
			return slug.trim();
		}
	}
	return undefined;
}

function detectWpCodeboxPublicFuzzCapabilities(options = {}) {
	if (options.publicCliReadiness || options.public_cli_readiness || options.readiness?.schema === WP_CODEBOX_FUZZ_RUNNER_READINESS_SCHEMA) {
		return normalizeWpCodeboxPublicFuzzCapabilitiesFromReadiness(options.publicCliReadiness || options.public_cli_readiness || options.readiness);
	}
	if (options.publicCliCapabilities || options.public_cli_capabilities) {
		return normalizeWpCodeboxPublicFuzzCapabilities(options.publicCliCapabilities || options.public_cli_capabilities);
	}
	if (options.capabilities?.public_cli || options.capabilities?.publicCli) {
		return normalizeWpCodeboxPublicFuzzCapabilities(options.capabilities.public_cli || options.capabilities.publicCli);
	}

	const manifestDescriptor = wpCodeboxPublicFuzzCapabilitiesFromRuntimeContract(options);
	if (manifestDescriptor) {
		return manifestDescriptor;
	}

	return normalizeWpCodeboxPublicFuzzCapabilities({
		commands: {},
		readiness: {
			schema: WP_CODEBOX_FUZZ_RUNNER_READINESS_SCHEMA,
			status: 'blocked',
			command_available: false,
			diagnostics: [{
				severity: 'error',
				code: 'wp_codebox_fuzz_explicit_public_descriptor_missing',
				message: 'WP Codebox public fuzz dispatch requires explicit runtime contract fields; Homeboy does not probe production runtimes for fuzz capabilities.',
			}],
		},
	});
}

function wpCodeboxPublicFuzzCapabilitiesFromRuntimeContract(options = {}) {
	const manifest = wpCodeboxRuntimeContractManifest(options);
	const commands = objectOrUndefined(manifest?.commands?.wordpressRuntime);
	const wordpressCapabilities = objectOrUndefined(manifest?.capabilities?.wordpressRuntime || manifest?.wordpressRuntime?.capabilities);
	const readiness = objectOrUndefined(manifest?.readiness?.wordpressRuntime || manifest?.wordpressRuntime?.readiness);
	if (!commands || !wordpressCapabilities || !readiness) {
		return null;
	}
	return normalizeWpCodeboxPublicFuzzCapabilities({
		commands: stripUndefined({
			[commands?.runFuzzSuite]: Boolean(commands?.runFuzzSuite),
			[commands?.runWorkload]: Boolean(commands?.runWorkload),
		}),
		capabilities: wordpressCapabilities?.capabilities || wordpressCapabilities?.runtime_capabilities || wordpressCapabilities?.runtimeCapabilities || wordpressCapabilities?.supports,
		runner_modes: wordpressCapabilities?.runner_modes || wordpressCapabilities?.runnerModes,
		readiness,
	});
}

function preflightWpCodeboxFuzzCapabilityContract(options = {}) {
	const request = options.request || options.taskRequest || options.task_request || null;
	const capabilities = detectWpCodeboxPublicFuzzCapabilities(options);
	const manifest = wpCodeboxRuntimeContractManifest(options);
	const wordpressRuntimeAbilities = objectOrUndefined(manifest?.abilities?.wordpressRuntime) || {};
	const commandManifest = buildWordPressFuzzCommandManifest();
	const suiteInput = wpCodeboxFuzzRequestInput(request, options);
	const plan = suiteInput.homeboy_fuzz_workload?.plan || suiteInput.homeboyFuzzWorkload?.plan || suiteInput.metadata?.workload?.plan || request?.input?.plan || options.plan || {};
	const requiredPlanContracts = requiredWpCodeboxContractsForFuzzPlan(plan);
	const destructiveReadiness = normalizeWpCodeboxDestructiveReadiness(capabilities.readiness, { request, suiteInput, plan });
	const requiredAbilities = { ...WP_CODEBOX_FUZZ_PUBLIC_ABILITIES };
	const requiredCommandMap = wpCodeboxWordPressRuntimeContracts({ runtimeContractManifest: manifest }).commands;
	const requiredCommands = requiredPublicCommandsForRequest(request, requiredPlanContracts);
	const missingContracts = [];
	const readinessStatus = capabilities.readiness?.status;
	const publicReadinessSatisfied = capabilities.readiness?.command_available !== false
		&& readinessStatus === 'ready'
		&& capabilities.readiness?.mode === 'runtime-backed';
	if (capabilities.readiness?.command_available === false) {
		missingContracts.push({
			type: 'explicit_public_descriptor',
			message: 'WP Codebox public fuzz dispatch requires explicit runtime contract fields; Homeboy does not probe production runtimes for fuzz capabilities.',
			readiness: capabilities.readiness,
		});
	}
	if (capabilities.readiness?.status === 'unsupported') {
		missingContracts.push({
			type: 'public_cli_readiness',
			message: 'WP Codebox fuzz readiness reports this runtime as unsupported for the requested fuzz contract.',
			unsupported_capabilities: normalizeArray(capabilities.readiness.unsupportedRequiredCapabilities || capabilities.readiness.unsupported_required_capabilities),
			readiness: capabilities.readiness,
		});
	}
	if (destructiveReadiness.required && !destructiveReadiness.ok) {
		missingContracts.push({
			type: 'destructive_readiness',
			missing_primitives: destructiveReadiness.missing_primitives,
			message: `Aggressive/destructive WordPress fuzzing requires WP Codebox runtime-backed readiness proof for: ${destructiveReadiness.missing_primitives.map((primitive) => primitive.label).join(', ')}.`,
			readiness: capabilities.readiness,
			policy: destructiveReadiness.policy,
		});
	}

	const missingManifestPaths = missingWpCodeboxFuzzRuntimeContractPaths(manifest);
	if (missingManifestPaths.length > 0) {
		missingContracts.push({
			type: 'runtime_contract_manifest',
			missing_paths: missingManifestPaths,
			message: `WP Codebox runtime contract manifest must declare required WordPress fuzz contracts: ${missingManifestPaths.join(', ')}.`,
		});
	}

	for (const command of requiredCommands) {
		if (!Object.values(requiredCommandMap).includes(command) || capabilities.commands?.[command] !== true) {
			missingContracts.push({
				type: 'public_cli_command',
				command,
				message: `WP Codebox runtime contract must explicitly declare public command \`${command}\` before Homeboy dispatches WordPress fuzz workloads.`,
			});
		}
	}

	const declaredRuntimeCapabilities = new Set(normalizeArray(capabilities.capabilities));
	for (const capability of normalizeArray(requiredPlanContracts.capabilities)) {
		if (!declaredRuntimeCapabilities.has(capability)) {
			missingContracts.push({
				type: 'runtime_capability',
				capability,
				message: `WP Codebox public fuzz runtime must declare \`${capability}\` before Homeboy dispatches matching WordPress fuzz workloads.`,
			});
		}
	}

	if (!publicReadinessSatisfied && wordpressRuntimeAbilities.runFuzzSuite !== requiredAbilities.runFuzzSuite) {
		missingContracts.push({
			type: 'ability',
			ability: requiredAbilities.runFuzzSuite,
			message: `WP Codebox runtime contract must declare \`${requiredAbilities.runFuzzSuite}\` for fuzz-suite dispatch.`,
		});
	}
	if (!publicReadinessSatisfied && wordpressRuntimeAbilities.runWorkload !== requiredAbilities.runWorkload) {
		missingContracts.push({
			type: 'ability',
			ability: requiredAbilities.runWorkload,
			message: `WP Codebox runtime contract must declare \`${requiredAbilities.runWorkload}\` for WordPress workload dispatch.`,
		});
	}

	return {
		schema: WP_CODEBOX_FUZZ_PREFLIGHT_SCHEMA,
		ok: missingContracts.length === 0,
		status: missingContracts.length === 0 ? 'passed' : 'failed',
		request_id: request?.task_id,
		capabilities,
		required: {
			commands: requiredCommands,
			abilities: requiredAbilities,
			capabilities: requiredPlanContracts.capabilities,
			runner_modes: requiredPlanContracts.runner_modes,
			destructive_readiness: destructiveReadiness.required ? WP_CODEBOX_DESTRUCTIVE_READINESS_REQUIREMENTS : undefined,
		},
		destructive_readiness: destructiveReadiness.required ? destructiveReadiness : undefined,
		command_manifest: commandManifest,
		missing_contracts: missingContracts,
		diagnostics: missingContracts.map((contract) => ({
			severity: 'error',
			code: contract.diagnostic?.code || `wp_codebox_fuzz_missing_${contract.type}`,
			message: contract.message,
			contract,
		})),
	};
}

function requiredPublicCommandsForRequest(request = {}, requiredPlanContracts = {}) {
	const ability = wpCodeboxFuzzRequestAbility(request);
	if (ability === DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY) {
		return [request.command].filter(Boolean);
	}
	if (ability === DEFAULT_FUZZ_SUITE_ABILITY) {
		const input = wpCodeboxFuzzRequestInput(request);
		const runnerMode = publicFuzzCliRunnerModeForRequest(request, { requiredPlanContracts });
		return unique([
			request.command,
			...normalizeArray(requiredPlanContracts.commands).filter((command) => runnerMode === 'runtime-backed' ? command !== 'run-wordpress-workload' : true),
			...(runnerMode === 'runtime-backed' || !suiteInputRequiresWorkloadCommand(input) ? [] : [wpCodeboxWordPressWorkloadRunCommand({ runtimeContractManifest: request.metadata?.runtime_contract_manifest })]),
		]);
	}
	return requiredPlanContracts.commands?.length > 0 ? unique(requiredPlanContracts.commands) : [...WP_CODEBOX_PUBLIC_CLI_COMMANDS];
}

function publicFuzzCliRunnerModeForRequest(request = {}, options = {}) {
	const explicit = nonEmptyString(options.runnerMode || options.runner_mode || request.runner_mode || request.runnerMode || request.input?.runner_mode || request.input?.runnerMode || request.input?.metadata?.runner_mode || request.input?.metadata?.runnerMode);
	if (explicit) {
		return explicit;
	}
	const requiredPlanContracts = options.requiredPlanContracts || options.required_plan_contracts || {};
	if (normalizeArray(requiredPlanContracts.runner_modes || requiredPlanContracts.runnerModes).includes('runtime-backed')) {
		return 'runtime-backed';
	}
	const input = wpCodeboxFuzzRequestInput(request, options);
	if (suiteInputRequiresWorkloadCommand(input) || suiteInputRequiresRuntimeBackedRunner(input)) {
		return 'runtime-backed';
	}
	return undefined;
}

function suiteInputRequiresRuntimeBackedRunner(input = {}) {
	return normalizeArray(input.cases).some((testCase) => {
		const commands = normalizeArray(testCase.phases?.action).map((step) => step?.command).filter(Boolean);
		const entrypoint = testCase.target?.entrypoint || testCase.target?.id;
		return String(entrypoint || '').startsWith('wordpress.')
			|| commands.some((command) => String(command || '').startsWith('wordpress.'))
			|| normalizeArray(testCase.inputs?.observation_surfaces || testCase.inputs?.observationSurfaces).some((surface) => ['browser', 'editor', 'page', 'admin'].some((keyword) => String(surface).includes(keyword)));
	});
}

function suiteInputRequiresWorkloadCommand(input = {}) {
	return normalizeArray(input.cases).some((testCase) => {
		const commands = normalizeArray(testCase.phases?.action).map((step) => step?.command).filter(Boolean);
		return testCase.metadata?.source_plan_case === true
			|| testCase.target?.entrypoint === 'wordpress.run-fuzz-case'
			|| commands.includes('wordpress.run-fuzz-case')
			|| commands.includes('wordpress.run-workload');
	});
}

function unique(values) {
	return [...new Set(normalizeArray(values).filter(Boolean))].sort();
}

function normalizeWpCodeboxDestructiveReadiness(readiness = {}, { request = {}, suiteInput = {}, plan = {} } = {}) {
	const required = wpCodeboxRequestRequiresDestructiveReadiness({ request, suiteInput, plan });
	const source = objectOrUndefined(readiness) || {};
	const capabilities = new Set(wordpressRuntimeCapabilitiesFromFuzzReadiness(source));
	const destructiveRequirements = objectOrUndefined(source.destructiveModeRequirements || source.destructive_mode_requirements) || {};
	const requiredBoundary = objectOrUndefined(destructiveRequirements.requiredSandboxBoundary || destructiveRequirements.required_sandbox_boundary) || {};
	const facts = {
		disposable_runtime: wpCodeboxReadinessDisposableRuntime(source, capabilities),
		disposable_sandbox_boundary: wpCodeboxReadinessDisposableSandboxBoundary(source, requiredBoundary, capabilities),
		destructive_permission: wpCodeboxReadinessDestructivePermission(source, requiredBoundary, capabilities),
		mutation_boundary: wpCodeboxReadinessMutationBoundary(source, destructiveRequirements, capabilities),
		external_side_effect_guardrail: wpCodeboxReadinessHasAny(source, capabilities, ['external-http-guardrail', 'external-side-effect-guardrail'], ['external_side_effect_guardrail', 'externalSideEffectGuardrail', 'external_http_guardrail', 'externalHttpGuardrail', 'external_http', 'externalHttp', 'http_guardrail', 'httpGuardrail']),
		artifact_export: wpCodeboxReadinessHasAny(source, capabilities, ['artifact-export'], ['artifact_export', 'artifactExport', 'artifacts_export', 'artifactsExport', 'export_artifacts', 'exportArtifacts']),
		teardown_discard: wpCodeboxReadinessTeardownDiscard(source, requiredBoundary, capabilities),
	};
	const missingPrimitives = required
		? WP_CODEBOX_DESTRUCTIVE_READINESS_REQUIREMENTS.filter((requirement) => facts[requirement.key] !== true)
		: [];
	return stripUndefined({
		schema: 'homeboy/wp-codebox-destructive-fuzz-readiness/v1',
		required,
		ok: !required || missingPrimitives.length === 0,
		provider: 'wp-codebox',
		status: source.status,
		mode: source.mode,
		facts,
		missing_primitives: missingPrimitives.length > 0 ? missingPrimitives : undefined,
		policy: required ? {
			mutation_mode: destructiveMutationModeFromRequest({ request, suiteInput, plan }),
			safety_class: strongestCodeboxReadinessSafetyClass({ request, suiteInput, plan }),
			required_sandbox_boundary: { disposable: true, destructivePermission: true, teardown: 'discard' },
			requirements: WP_CODEBOX_DESTRUCTIVE_READINESS_REQUIREMENTS,
		} : undefined,
	});
}

function wpCodeboxReadinessDisposableSandboxBoundary(readiness = {}, requiredBoundary = {}, capabilities = new Set()) {
	return (requiredBoundary.disposable === true && requiredBoundary.destructivePermission === true)
		|| capabilities.has('disposable-sandbox-boundary')
		|| booleanAtAnyPath(readiness, ['disposableSandboxBoundary.disposable', 'disposable_sandbox_boundary.disposable', 'sandboxBoundary.disposable', 'sandbox_boundary.disposable']);
}

function wpCodeboxReadinessDestructivePermission(readiness = {}, requiredBoundary = {}, capabilities = new Set()) {
	return requiredBoundary.destructivePermission === true
		|| requiredBoundary.destructive_permission === true
		|| capabilities.has('destructive-permission')
		|| booleanAtAnyPath(readiness, ['destructivePermission', 'destructive_permission', 'disposableSandboxBoundary.destructivePermission', 'disposable_sandbox_boundary.destructive_permission', 'sandboxBoundary.destructivePermission', 'sandbox_boundary.destructive_permission']);
}

function wpCodeboxReadinessMutationBoundary(readiness = {}, destructiveRequirements = {}, capabilities = new Set()) {
	const requiredArtifacts = new Set(normalizeArray(destructiveRequirements.requiredArtifacts || destructiveRequirements.required_artifacts));
	return capabilities.has('mutation-isolation-artifact')
		|| capabilities.has('delete-boundary-artifact')
		|| requiredArtifacts.has('mutation-isolation-artifact')
		|| requiredArtifacts.has('delete-boundary-artifact')
		|| booleanAtAnyPath(readiness, ['mutationBoundary', 'mutation_boundary', 'artifacts.mutation_isolation_artifact', 'artifacts.delete_boundary_artifact']);
}

function wpCodeboxReadinessTeardownDiscard(readiness = {}, requiredBoundary = {}, capabilities = new Set()) {
	return requiredBoundary.teardown === 'discard'
		|| requiredBoundary.teardown === 'destroy'
		|| capabilities.has('sandbox-isolation-proof')
		|| ['discard', 'destroy'].includes(String(readiness.disposableSandboxBoundary?.teardown || readiness.disposable_sandbox_boundary?.teardown || readiness.sandboxBoundary?.teardown || readiness.sandbox_boundary?.teardown || readiness.teardown || '').toLowerCase());
}

function wpCodeboxRequestRequiresDestructiveReadiness({ request = {}, suiteInput = {}, plan = {} } = {}) {
	return destructiveMutationModeFromRequest({ request, suiteInput, plan }) === 'aggressive-isolated'
		|| ['isolated_mutation', 'destructive'].includes(strongestCodeboxReadinessSafetyClass({ request, suiteInput, plan }))
		|| wpCodeboxContainsDestructiveCase(suiteInput)
		|| wpCodeboxContainsDestructiveCase(plan);
}

function destructiveMutationModeFromRequest({ request = {}, suiteInput = {}, plan = {} } = {}) {
	for (const value of [
		request.mutation_mode,
		request.mutationMode,
		request.input?.mutation_mode,
		request.input?.mutationMode,
		request.input?.metadata?.mutation_mode,
		request.input?.metadata?.mutationMode,
		suiteInput.mutation_mode,
		suiteInput.mutationMode,
		suiteInput.metadata?.mutation_mode,
		suiteInput.metadata?.mutationMode,
		plan.mutation_mode,
		plan.mutationMode,
		plan.metadata?.mutation_mode,
		plan.metadata?.mutationMode,
	]) {
		const normalized = normalizeMutationMode(value);
		if (normalized) {
			return normalized;
		}
	}
	return undefined;
}

function strongestCodeboxReadinessSafetyClass({ request = {}, suiteInput = {}, plan = {} } = {}) {
	const candidates = [
		codeboxReadinessSafetyClass(request),
		codeboxReadinessSafetyClass(request.input),
		codeboxReadinessSafetyClass(request.input?.metadata),
		codeboxReadinessSafetyClass(suiteInput),
		codeboxReadinessSafetyClass(suiteInput.metadata),
		codeboxReadinessSafetyClass(plan),
		codeboxReadinessSafetyClass(plan.metadata),
		...normalizeArray(suiteInput.cases).flatMap((testCase) => [codeboxReadinessSafetyClass(testCase), codeboxReadinessSafetyClass(testCase.metadata), codeboxReadinessSafetyClass(testCase.input)]),
		...normalizeArray(plan.targets).flatMap((target) => [
			codeboxReadinessSafetyClass(target),
			codeboxReadinessSafetyClass(target.metadata),
			...normalizeArray(target.cases).flatMap((testCase) => [codeboxReadinessSafetyClass(testCase), codeboxReadinessSafetyClass(testCase.metadata), codeboxReadinessSafetyClass(testCase.input)]),
		]),
	].filter(Boolean);
	return strongestReadinessSafetyClass(candidates);
}

function codeboxReadinessSafetyClass(source = {}) {
	if (!source || typeof source !== 'object') {
		return undefined;
	}
	const safety = objectOrUndefined(source.safety) || {};
	const explicit = source.safety_class || source.safetyClass || safety.safety_class || safety.safetyClass || safety.class || safety.level || safety.mutation || source.mutation;
	const explicitClass = normalizeReadinessSafetyClass(explicit);
	if (explicitClass) {
		return explicitClass;
	}
	if (source.destructive === true || safety.destructive === true) {
		return 'destructive';
	}
	if (source.mutates === true || safety.mutates === true || normalizeArray(source.destructive_reasons || source.destructiveReasons || source.destructive_reason || source.destructiveReason).length > 0) {
		return 'isolated_mutation';
	}
	const method = String(source.method || source.operation?.method || source.input?.method || '').toUpperCase();
	if (['POST', 'PUT', 'PATCH', 'DELETE'].includes(method)) {
		return method === 'DELETE' ? 'destructive' : 'isolated_mutation';
	}
	return undefined;
}

function wpCodeboxContainsDestructiveCase(source = {}) {
	return normalizeArray(source.cases).some((testCase) => codeboxReadinessSafetyClass(testCase) || codeboxReadinessSafetyClass(testCase.metadata) || codeboxReadinessSafetyClass(testCase.input))
		|| normalizeArray(source.targets).some((target) => codeboxReadinessSafetyClass(target) || codeboxReadinessSafetyClass(target.metadata) || wpCodeboxContainsDestructiveCase(target));
}

function normalizeReadinessSafetyClass(value) {
	const label = String(value || '').trim().toLowerCase().replace(/[\s.-]+/g, '_');
	if (!label) {
		return undefined;
	}
	if (['read_only', 'readonly', 'safe', 'non_mutating', 'none'].includes(label)) {
		return 'read_only';
	}
	if (['idempotent', 'repeatable'].includes(label)) {
		return 'idempotent';
	}
	if (['isolated_mutation', 'isolated', 'mutation', 'mutating', 'write', 'requires_isolated_editor_draft', 'requires_explicit_opt_in'].includes(label)) {
		return 'isolated_mutation';
	}
	if (['destructive', 'delete', 'dangerous', 'aggressive'].includes(label)) {
		return 'destructive';
	}
	return undefined;
}

function strongestReadinessSafetyClass(candidates = []) {
	const rank = { read_only: 0, idempotent: 1, isolated_mutation: 2, destructive: 3 };
	return candidates.reduce((strongest, candidate) => (rank[candidate] > rank[strongest] ? candidate : strongest), 'read_only');
}

function normalizeMutationMode(value) {
	const label = String(value || '').trim().toLowerCase().replace(/_/g, '-');
	if (label === 'destructive-isolated') {
		return 'aggressive-isolated';
	}
	return ['isolated', 'aggressive-isolated', 'read-only', 'destructive-deny'].includes(label) ? label : undefined;
}

function wpCodeboxReadinessDisposableRuntime(readiness = {}, capabilities = new Set()) {
	return readiness.status === 'ready'
		&& readiness.mode === 'runtime-backed'
		&& (capabilities.has('disposable-runtime')
			|| capabilities.has('runtime-isolation')
			|| booleanAtAnyPath(readiness, ['disposable', 'sandbox.disposable', 'runtime.disposable', 'isolation.disposable', 'isolation.runtime_backed', 'isolation.runtimeBacked', 'isolation.sandboxed', 'runtime_isolation', 'runtimeIsolation', 'sandbox.isolated']));
}

function wpCodeboxReadinessHasAny(readiness = {}, capabilities = new Set(), capabilityNames = [], paths = []) {
	if (capabilityNames.some((capability) => capabilities.has(capability))) {
		return true;
	}
	return paths.some((pathName) => booleanAtAnyPath(readiness, [pathName, `rollback.${pathName}`, `isolation.${pathName}`, `guardrails.${pathName}`, `artifacts.${pathName}`]));
}

function booleanAtAnyPath(source = {}, paths = []) {
	return paths.some((pathName) => {
		const value = valueAtPath(source, pathName);
		return value === true || value === 'true' || value === 'supported' || value === 'enabled' || value === 'available' || value === 'ready';
	});
}

function normalizeWpCodeboxPublicFuzzCapabilities(input = {}) {
	const commands = objectOrUndefined(input.commands) || input;
	const runtimeCapabilities = normalizeWordPressFuzzRuntimeCapabilities(input.capabilities || input.runtime_capabilities || input.runtimeCapabilities || input.supports || []);
	const readiness = objectOrUndefined(input.readiness) || objectOrUndefined(input.runtime_readiness) || objectOrUndefined(input.runtimeReadiness);
	return {
		schema: 'homeboy/wp-codebox-public-fuzz-capabilities/v1',
		commands: Object.fromEntries(WP_CODEBOX_PUBLIC_CLI_COMMANDS.map((command) => [command, commands[command] === true])),
		capabilities: runtimeCapabilities.capabilities,
		runner_modes: Object.fromEntries(WP_CODEBOX_FUZZ_PUBLIC_RUNNER_MODES.map((runnerMode) => [runnerMode, input.runner_modes?.[runnerMode] !== false && input.runnerModes?.[runnerMode] !== false])),
		readiness,
	};
}

function normalizeWpCodeboxPublicFuzzCapabilitiesFromReadiness(readiness = {}) {
	const source = objectOrUndefined(readiness) || {};
	const nestedCapabilities = objectOrUndefined(source.capabilities) || {};
	const commands = new Set(normalizeArray(nestedCapabilities.commands));
	return normalizeWpCodeboxPublicFuzzCapabilities({
		commands: {
			'run-fuzz-suite': source.entrypoint === 'run-fuzz-suite' || String(source.entrypoint || '').startsWith('run-fuzz-suite'),
			'run-wordpress-workload': commands.has('wordpress.run-workload'),
		},
		capabilities: wordpressRuntimeCapabilitiesFromFuzzReadiness(source),
		runner_modes: { [source.mode || 'runtime-backed']: source.status !== 'unsupported' },
		readiness: {
			...source,
			command_available: source.schema === WP_CODEBOX_FUZZ_RUNNER_READINESS_SCHEMA,
		},
	});
}

function wordpressRuntimeCapabilitiesFromFuzzReadiness(readiness = {}) {
	const capabilities = objectOrUndefined(readiness.capabilities) || {};
	const destructiveRequirements = objectOrUndefined(readiness.destructiveModeRequirements || readiness.destructive_mode_requirements) || {};
	const requiredBoundary = objectOrUndefined(destructiveRequirements.requiredSandboxBoundary || destructiveRequirements.required_sandbox_boundary) || {};
	const requiredArtifacts = new Set(normalizeArray(destructiveRequirements.requiredArtifacts || destructiveRequirements.required_artifacts));
	const runtimeActions = new Set(normalizeArray(capabilities.runtimeActionTypes || capabilities.runtime_action_types));
	const commands = new Set(normalizeArray(capabilities.commands));
	const declaredCapabilities = normalizeWordPressFuzzRuntimeCapabilities([
		...normalizeArray(readiness.capabilities?.capabilities),
		...normalizeArray(readiness.capabilities?.supports),
		...normalizeArray(readiness.capabilities?.runtime_capabilities || readiness.capabilities?.runtimeCapabilities),
		...normalizeArray(readiness.runtime_capabilities || readiness.runtimeCapabilities || readiness.supports),
	]).capabilities;
	return unique([
		...declaredCapabilities,
		...(runtimeActions.has('crud_operation') || commands.has('wordpress.crud-operation') ? ['crud'] : []),
		...(runtimeActions.has('rest_request') || commands.has('wordpress.rest-request') ? ['rest'] : []),
		...(runtimeActions.has('admin_page') || commands.has('wordpress.admin-page-load') ? ['admin'] : []),
		...(runtimeActions.has('browser') || runtimeActions.has('browser_probe') || runtimeActions.has('page') || commands.has('wordpress.browser-actions') || commands.has('wordpress.browser-probe') || commands.has('wordpress.frontend-page-load') ? ['browser'] : []),
		...(runtimeActions.has('editor_open') ? ['block-editor'] : []),
		...(runtimeActions.has('php') || runtimeActions.has('wp_cli') || commands.has('wordpress.run-php') || commands.has('wordpress.wp-cli') ? ['database', 'query-observation', 'sequence'] : []),
		...(readiness.mode === 'runtime-backed' && booleanAtAnyPath(readiness, ['disposable', 'sandbox.disposable', 'runtime.disposable', 'isolation.disposable', 'isolation.runtime_backed', 'isolation.runtimeBacked', 'isolation.sandboxed', 'runtime_isolation', 'runtimeIsolation', 'sandbox.isolated']) ? ['disposable-runtime', 'runtime-isolation'] : []),
		...(requiredBoundary.disposable === true ? ['disposable-sandbox-boundary'] : []),
		...(requiredBoundary.destructivePermission === true || requiredBoundary.destructive_permission === true ? ['destructive-permission'] : []),
		...(requiredArtifacts.has('mutation-isolation-artifact') ? ['mutation-isolation-artifact'] : []),
		...(requiredArtifacts.has('delete-boundary-artifact') ? ['delete-boundary-artifact'] : []),
		...(capabilities.capabilities?.includes?.('wordpress-runtime:sandbox-isolation-proof') || declaredCapabilities.includes('sandbox-isolation-proof') || requiredArtifacts.has('sandbox-isolation-proof') ? ['sandbox-isolation-proof'] : []),
		...(wpCodeboxReadinessHasAny(readiness, new Set(declaredCapabilities), [], ['external_http_guardrail', 'externalHttpGuardrail', 'external_http', 'externalHttp', 'http_guardrail', 'httpGuardrail']) ? ['external-http-guardrail'] : []),
		...(wpCodeboxReadinessHasAny(readiness, new Set(declaredCapabilities), [], ['external_side_effect_guardrail', 'externalSideEffectGuardrail']) ? ['external-side-effect-guardrail'] : []),
		...(wpCodeboxReadinessHasAny(readiness, new Set(declaredCapabilities), [], ['artifact_export', 'artifactExport', 'artifacts_export', 'artifactsExport', 'export_artifacts', 'exportArtifacts']) ? ['artifact-export'] : []),
	]);
}

function publicFuzzCliCommandForRequest(request = {}, capabilities = {}) {
	if (request.command && capabilities.commands?.[request.command]) {
		return request.command;
	}
	return undefined;
}

function wpCodeboxFuzzRequestAbility(request = {}) {
	return request.ability || request.executor?.config?.runtime_task?.ability;
}

function wpCodeboxFuzzRequestInput(request = {}, options = {}) {
	return request.input || request.executor?.config?.runtime_task?.input || options.input || {};
}

function wpCodeboxFuzzRequestRuntimeRequirements(request = {}) {
	return request.runtime_requirements || request.executor?.config?.runtime_requirements || request.executor?.config?.runtimeRequirements;
}

function unsupportedWpCodeboxPublicFuzzResult({ request = {}, capabilities = {}, preflight } = {}) {
	const diagnostics = preflight?.diagnostics?.length > 0 ? preflight.diagnostics : [{
		severity: 'warning',
		code: 'wp_codebox_public_fuzz_cli_unsupported',
		message: 'WP Codebox public fuzz execution is unavailable: neither `wp codebox run-fuzz-suite` nor `wp codebox run-wordpress-workload` is exposed by this runtime.',
		capabilities,
	}];
	return {
		schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
		request_id: request.task_id,
		status: 'skipped',
		metadata: { readiness: { level: 'declared' }, unsupported: true, preflight },
		diagnostics,
	};
}

async function runWpCodeboxPublicCli(command, input, options = {}) {
	return createCodeboxClient(options).runPublicJsonCommand(command, input, options);
}

function wpCodeboxCommandFromFuzzAbility(ability, options = {}) {
	if (ability === wpCodeboxWordPressWorkloadRunAbility(options)) {
		return wpCodeboxWordPressWorkloadRunCommand(options);
	}
	if (ability === wpCodeboxFuzzSuiteAbility(options)) {
		return wpCodeboxFuzzSuiteCommand(options);
	}
	return undefined;
}

function wpCodeboxPublicCliBin(options = {}) {
	if (options.wpCliBin || options.wp_cli_bin) {
		return options.wpCliBin || options.wp_cli_bin;
	}
	const env = { ...process.env, ...(options.env || {}) };
	if (env.wpCliBin || env.wp_cli_bin || env.HOMEBOY_WP_CLI_BIN || env.WP_CLI_BIN) {
		return env.wpCliBin || env.wp_cli_bin || env.HOMEBOY_WP_CLI_BIN || env.WP_CLI_BIN;
	}
	return createCodeboxClient(options).publicCliBin(options);
}

function parseWpCodeboxPublicCliJson(stdout, { request = {}, command } = {}) {
	try {
		return JSON.parse(String(stdout || '').trim() || '{}');
	} catch (error) {
		return {
			schema: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			request_id: request.task_id,
			status: 'failed',
			diagnostics: [{
				severity: 'error',
				code: 'wp_codebox_public_cli_invalid_json',
				message: `wp codebox ${command} did not return JSON output.`,
				error: error.message,
				stdout: String(stdout || ''),
			}],
		};
	}
}

function normalizeWpCodeboxFuzzSuiteResult(result = {}, context = {}) {
	const source = normalizeWpCodeboxFuzzResultSource(result?.json || result?.result || result?.output || result);
	let status = source?.status || source?.outcome?.status || result?.status || '';
	const artifacts = normalizeWpCodeboxFuzzArtifacts(source, result);
	const coverageSummary = normalizeCoverageSummary(source?.coverage_summary || source?.coverageSummary || source?.coverage?.summary);
	const derivedArtifacts = normalizeDerivedFuzzArtifacts(source, artifacts);
	const observationSet = normalizeCodeboxFuzzObservationSet(source, { request: context.request });
	const coverageGaps = normalizeCoverageGaps([
		...normalizeArray(source?.coverage_gaps || source?.coverageGaps || source?.coverage?.gaps),
		...normalizeArray(derivedArtifacts.coverage_gap_reports).flatMap((report) => normalizeArray(report.coverage_gaps)),
	]);
	const hotspotSummary = normalizeFuzzHotspotSummary(source?.hotspot_summary || source?.hotspotSummary || source?.hotspots || source?.performance_hotspots || source?.performanceHotspots || derivedArtifacts.hotspot_summary || fuzzHotspotSummaryFromObservationSet(observationSet, { provider: 'wp-codebox', taskId: source?.request_id || source?.requestId || context.request?.task_id }), { provider: 'wp-codebox', taskId: source?.request_id || source?.requestId || context.request?.task_id });
	const normalizedResult = normalizeEmbeddedWordPressFuzzResult(source);
	const contractFailures = wpCodeboxFuzzContractFailures({ source, result, context, artifacts, coverageSummary, normalizedResult, hotspotSummary, derivedArtifacts });
	if (contractFailures.length > 0 && ['succeeded', 'success', 'passed', 'ok'].includes(String(status).toLowerCase())) {
		status = 'failed';
	}
	const failures = [...normalizeArray(source?.failures || source?.errors || source?.diagnostics), ...contractFailures];
	const runtimeTaskResult = normalizeWordPressFuzzRuntimeTaskResult({
		...source,
		status,
		artifacts,
		failures,
		hotspot_summary: hotspotSummary,
		observation_set: observationSet,
		provider_result: source,
	}, { provider: 'wp-codebox', taskId: context.request?.task_id });
	const observation = buildWordPressFuzzObservation({
		source,
		status,
		artifacts,
		coverageSummary,
		hotspotSummary,
		normalizedResult,
		failures,
		context,
	});
	return stripUndefined({
		schema: WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA,
		delegated_schema: WP_CODEBOX_FUZZ_SUITE_SCHEMA,
		result_schema: source?.schema || result?.schema || WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
		request_id: source?.request_id || source?.requestId || context.request?.task_id,
		status,
		succeeded: status ? ['succeeded', 'success', 'passed', 'ok'].includes(String(status).toLowerCase()) : undefined,
		observation,
		coverage: source?.coverage,
		coverage_summary: coverageSummary,
		coverage_gaps: coverageGaps,
		observation_set: observationSet,
		hotspot_summary: hotspotSummary,
		derived_artifacts: derivedArtifacts.artifacts.length > 0 ? derivedArtifacts : undefined,
		wordpress_fuzz_result: normalizedResult,
		artifacts,
		failures,
		runtime_task_result: runtimeTaskResult,
		metadata: stripUndefined({
			...(objectOrUndefined(source?.metadata) || {}),
			suite: objectOrUndefined(source?.suite),
			summary: objectOrUndefined(source?.summary),
			runtime_task_request: objectOrUndefined(context.runtimeRequest),
		}),
	});
}

function normalizeCodeboxFuzzObservationSet(source = {}, context = {}) {
	return normalizeFuzzObservationSet({
		id: source?.observation_set?.id || source?.observationSet?.id || source?.request_id || source?.requestId || context.request?.task_id,
		observations: [
			...normalizeArray(source?.observation_set?.observations || source?.observationSet?.observations || source?.observations),
			...normalizeArray(source?.measurements),
			...normalizeArray(source?.performance?.measurements || source?.performance_measurements || source?.performanceMeasurements),
			...normalizeArray(source?.queries || source?.query_measurements || source?.queryMeasurements || source?.query_data || source?.queryData).map((entry) => objectOrUndefined(entry) ? { family: 'query', ...entry } : entry),
			...normalizeArray(source?.actions || source?.action_measurements || source?.actionMeasurements).map((entry) => objectOrUndefined(entry) ? { family: 'action', ...entry } : entry),
			...normalizeArray(source?.resources || source?.resource_measurements || source?.resourceMeasurements).map((entry) => objectOrUndefined(entry) ? { family: 'resource', ...entry } : entry),
			...normalizeArray(source?.timings || source?.timing_measurements || source?.timingMeasurements).map((entry) => objectOrUndefined(entry) ? { family: 'timing', ...entry } : entry),
			...normalizeArray(source?.counters || source?.counter_measurements || source?.counterMeasurements).map((entry) => objectOrUndefined(entry) ? { family: 'counter', ...entry } : entry),
			...deleteBoundaryArtifactObservations(source),
			...wordpressFuzzResultCaseObservations(source?.wordpress_fuzz_result || source?.wordpressFuzzResult || source?.normalized_result || source?.normalizedResult),
		],
		metadata: stripUndefined({
			wp_codebox_result_schema: source?.schema,
		}),
	}, { provider: 'wp-codebox', taskId: source?.request_id || source?.requestId || context.request?.task_id });
}

function deleteBoundaryArtifactObservations(source = {}) {
	return normalizeArray(source?.delete_boundary_artifacts || source?.deleteBoundaryArtifacts)
		.map((artifact) => {
			if (!objectOrUndefined(artifact)) {
				return undefined;
			}
			return stripUndefined({
				family: 'mutation-boundary',
				subject: 'delete_boundary',
				metric: 'delete_boundary_artifact',
				value: ['failed', 'error', 'missing'].includes(String(artifact.status || '').toLowerCase()) ? 0 : 1,
				case_id: artifact.case_id || artifact.caseId,
				operation_id: artifact.operation_id || artifact.operationId,
				fingerprint: artifact.fingerprint || artifact.id || artifact.name || artifact.path,
				status: artifact.status,
				metadata: stripUndefined({
					artifact_name: artifact.name || artifact.id,
					path: artifact.path || artifact.file || artifact.artifact || artifact.uri,
					schema: artifact.schema || artifact.artifact_schema || artifact.artifactSchema || artifact.metadata?.schema,
				}),
			});
		})
		.filter(Boolean);
}

function wordpressFuzzResultCaseObservations(result = {}) {
	return normalizeArray(result?.cases).flatMap((testCase) => {
		const common = {
			case_id: testCase.id || testCase.case_id || testCase.caseId,
			target_id: testCase.target_id || testCase.targetId || testCase.surface_id || testCase.surfaceId,
			operation_id: testCase.operation_id || testCase.operationId,
		};
		return [
			...objectMetricObservations(testCase.db_query || testCase.dbQuery, { ...common, family: 'query', subject: 'db_query' }),
			...objectMetricObservations(testCase.performance_metrics || testCase.performanceMetrics, { ...common, family: 'timing', subject: 'performance' }),
		];
	});
}

function objectMetricObservations(metrics, defaults = {}) {
	if (!objectOrUndefined(metrics)) {
		return [];
	}
	return Object.entries(metrics).flatMap(([metric, value]) => Number.isFinite(Number(value)) ? [{ ...defaults, metric, value }] : []);
}

function wpCodeboxFuzzContractFailures({ source = {}, result = {}, context = {}, artifacts = [], coverageSummary, normalizedResult, hotspotSummary, derivedArtifacts }) {
	if (wpCodeboxFuzzAllowsEmpty(source, context)) {
		return [];
	}

	const requiredArtifacts = wpCodeboxFuzzRequiredArtifactDeclarations(context.request || context.taskRequest || result.request || {});
	const caseCount = wpCodeboxFuzzCaseCount(source, normalizedResult);
	const expectsCoverage = wpCodeboxFuzzExpectsCoverage(source, context, coverageSummary);
	const failures = [];

	if (caseCount === 0 && (requiredArtifacts.length > 0 || expectsCoverage)) {
		failures.push({
			severity: 'error',
			code: 'wp_codebox_fuzz_empty_cases_for_declared_contract',
			message: 'WP Codebox fuzz result produced no cases for a request that declares required artifacts or non-empty coverage.',
			required_artifacts: requiredArtifacts.map((artifact) => artifact.name || artifact.path || artifact.semantic_key).filter(Boolean),
			expects_coverage: expectsCoverage,
		});
	}

	if (requiredArtifacts.length > 0 && artifacts.length === 0) {
		failures.push({
			severity: 'error',
			code: 'wp_codebox_fuzz_required_artifacts_missing',
			message: 'WP Codebox fuzz result produced no artifacts for a request with required artifact declarations.',
			required_artifacts: requiredArtifacts.map((artifact) => artifact.name || artifact.path || artifact.semantic_key).filter(Boolean),
		});
	}

	const requiredOutputFailures = requiredFuzzOutputFailures({ source, context, artifacts, normalizedResult, hotspotSummary });
	failures.push(...requiredOutputFailures);
	failures.push(...mutationLifecycleContractFailures({ source, context, artifacts, normalizedResult }));
	failures.push(...destructiveDisposableLifecycleFailures({ source, context, artifacts, normalizedResult }));
	failures.push(...validateWordPressFuzzPostprocessOutputs({ source, context, artifacts, derivedArtifacts, hotspotSummary, normalizedResult }));

	return failures;
}

function destructiveDisposableLifecycleFailures({ source = {}, context = {}, artifacts = [], normalizedResult } = {}) {
	const planCases = requestPlanCases(context.request || context.taskRequest || {});
	const resultCases = new Map(normalizeArray(normalizedResult?.cases || source.cases || source.fuzz_cases || source.fuzzCases).map((testCase) => [testCase.id || testCase.case_id || testCase.caseId, testCase]));
	const destructiveCases = planCases.filter((planCase) => fuzzCaseRequiresDisposableLifecycle(planCase, resultCases.get(planCase.id || planCase.case_id || planCase.caseId)));
	if (destructiveCases.length === 0 || hasSuccessfulDisposableLifecycleArtifacts({ artifacts, source, normalizedResult })) {
		return [];
	}
	return [{
		severity: 'error',
		code: 'wp_codebox_fuzz_disposable_lifecycle_artifacts_missing',
		message: 'Aggressive/destructive WP Codebox fuzz success is missing required disposable sandbox lifecycle artifacts.',
		missing_artifact_keys: ['fuzz.disposable.sandbox_isolation_proof', 'fuzz.mutation.isolation', 'fuzz.delete.boundary'],
		case_ids: destructiveCases.map((testCase) => testCase.id || testCase.case_id || testCase.caseId).filter(Boolean),
	}];
}

function fuzzCaseRequiresDisposableLifecycle(planCase = {}, resultCase = {}) {
	const status = String(resultCase?.status || '').toLowerCase();
	if (['skipped', 'skip', 'planned', 'plan_only'].includes(status)) {
		return false;
	}
	const metadata = objectOrUndefined(planCase.metadata) || {};
	const safety = objectOrUndefined(planCase.safety || metadata.safety) || {};
	const lifecycle = normalizeWordPressFuzzMutationLifecycleContract(metadata.mutation_lifecycle || metadata.mutationLifecycle || planCase.mutation_lifecycle || planCase.mutationLifecycle);
	return Boolean(
		lifecycle?.delete_boundary_required
		|| lifecycle?.required_evidence?.some((entry) => entry.semantic_key === 'fuzz.delete.boundary' || entry.kind === 'delete-boundary' || entry.kind === 'sandbox-isolation-proof')
		|| planCase.destructive === true
		|| metadata.destructive === true
		|| safety.destructive === true
		|| safety.level === 'destructive'
		|| metadata.aggressive === true
		|| String(metadata.mode || metadata.fuzz_mode || metadata.fuzzMode || '').toLowerCase() === 'aggressive'
		|| normalizeArray(planCase.destructive_reasons || planCase.destructiveReasons || metadata.destructive_reasons || metadata.destructiveReasons).length > 0
	);
}

function hasSuccessfulDisposableLifecycleArtifacts({ artifacts = [], source = {}, normalizedResult = {} } = {}) {
	const candidates = [
		...normalizeArray(artifacts),
		...normalizeArray(source.sandboxIsolationProof || source.sandbox_isolation_proof),
		...normalizeArray(source.mutationIsolationArtifact || source.mutation_isolation_artifact),
		...normalizeArray(source.deleteBoundaryArtifact || source.delete_boundary_artifact),
		...normalizeArray(normalizedResult?.cases).flatMap((testCase) => [
			testCase.metadata?.sandboxIsolationProof,
			testCase.metadata?.sandbox_isolation_proof,
			testCase.metadata?.mutationIsolationArtifact,
			testCase.metadata?.mutation_isolation_artifact,
			testCase.metadata?.deleteBoundaryArtifact,
			testCase.metadata?.delete_boundary_artifact,
		]),
	].filter(Boolean);
	const hasSandboxProof = candidates.some((artifact) => disposableLifecycleArtifactMatches(artifact, ['sandbox_isolation_proof']));
	const hasMutationBoundary = candidates.some((artifact) => disposableLifecycleArtifactMatches(artifact, ['mutation_isolation_artifact', 'delete_boundary_artifact']));
	return hasSandboxProof && hasMutationBoundary;
}

function disposableLifecycleArtifactMatches(artifact = {}, allowedRoles = []) {
	if (!objectOrUndefined(artifact)) {
		return false;
	}
	const allowed = new Set(allowedRoles);
	const role = artifact.role || normalizeFuzzArtifactRole(artifact.name || artifact.semantic_key || artifact.semanticKey || artifact.path);
	const semanticKey = artifact.semantic_key || artifact.semanticKey || artifact.metadata?.semantic_key || artifact.metadata?.semanticKey;
	const schema = artifact.schema || artifact.metadata?.schema;
	const kind = artifact.kind || artifact.artifactKind || artifact.artifact_kind || artifact.metadata?.kind;
	const status = String(artifact.status || artifact.metadata?.status || '').toLowerCase();
	return ['failed', 'errored', 'error', 'missing'].includes(status) === false
		&& ((allowed.has(role))
			|| (allowed.has('sandbox_isolation_proof') && (schema === 'wp-codebox/sandbox-isolation-proof/v1' || kind === 'sandbox-isolation-proof' || semanticKey === 'fuzz.disposable.sandbox_isolation_proof'))
			|| (allowed.has('mutation_isolation_artifact') && (schema === 'wp-codebox/mutation-isolation-artifact/v1' || kind === 'mutation-isolation-artifact' || semanticKey === 'fuzz.mutation.isolation'))
			|| (allowed.has('delete_boundary_artifact') && (schema === 'wp-codebox/delete-boundary-artifact/v1' || kind === 'delete-boundary-artifact' || semanticKey === 'fuzz.delete.boundary')));
}

function mutationLifecycleContractFailures({ source = {}, context = {}, artifacts = [], normalizedResult } = {}) {
	const planCases = requestPlanCases(context.request || context.taskRequest || {});
	if (planCases.length === 0) {
		return [];
	}
	const resultCases = new Map(normalizeArray(normalizedResult?.cases || source.cases || source.fuzz_cases || source.fuzzCases).map((testCase) => [testCase.id || testCase.case_id || testCase.caseId, testCase]));
	return planCases.flatMap((planCase) => {
		const contract = normalizeWordPressFuzzMutationLifecycleContract(planCase.metadata?.mutation_lifecycle || planCase.metadata?.mutationLifecycle || planCase.mutation_lifecycle || planCase.mutationLifecycle);
		if (!contract) {
			return [];
		}
		const caseId = planCase.id || planCase.case_id || planCase.caseId;
		const resultCase = resultCases.get(caseId);
		if (!resultCase || String(resultCase.status || '').toLowerCase() === 'skipped') {
			return [];
		}
		return wordpressFuzzMutationLifecycleDiagnosticsForCase({
			...resultCase,
			metadata: { ...(objectOrUndefined(resultCase.metadata) || {}), mutation_lifecycle: contract },
		}, artifacts).map((diagnostic) => ({
			...diagnostic,
			code: 'wp_codebox_fuzz_mutation_lifecycle_evidence_missing',
			message: 'WP Codebox fuzz result claimed WordPress mutation execution without required disposable sandbox lifecycle evidence.',
		}));
	});
}

function requestPlanCases(request = {}) {
	const input = wpCodeboxFuzzRequestInput(request);
	const plan = input.homeboy_fuzz_workload?.plan || input.homeboyFuzzWorkload?.plan || input.metadata?.workload?.plan || input.plan || request.input?.plan || {};
	return [
		...normalizeArray(plan.targets).flatMap((target) => normalizeArray(target.cases)),
		...normalizeArray(input.cases),
	];
}

function buildWordPressFuzzObservation({ source = {}, status, artifacts = [], coverageSummary, hotspotSummary, normalizedResult, failures = [], context = {} } = {}) {
	const succeeded = status ? ['succeeded', 'success', 'passed', 'ok'].includes(String(status).toLowerCase()) : undefined;
	const summary = normalizeObservationSummary(source, normalizedResult, coverageSummary);
	return stripUndefined({
		schema: WORDPRESS_FUZZ_OBSERVATION_SCHEMA,
		version: 1,
		id: source?.suite?.id || source?.request_id || source?.requestId || context.request?.task_id || normalizedResult?.id,
		status,
		succeeded,
		source: stripUndefined({
			provider: 'wp-codebox',
			result_schema: source?.schema || WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
			suite_id: source?.suite?.id,
			request_id: source?.request_id || source?.requestId || context.request?.task_id,
		}),
		summary,
		metrics: normalizeObservationMetrics({ coverageSummary, hotspotSummary, normalizedResult }),
		artifacts,
		failures: failures.length > 0 ? failures : undefined,
		normalized_result: normalizedResult,
	});
}

function normalizeObservationSummary(source = {}, normalizedResult, coverageSummary) {
	const sourceSummary = objectOrUndefined(source?.summary) || {};
	const normalizedSummary = objectOrUndefined(normalizedResult?.summary) || {};
	return stripUndefined({
		total: numberOrUndefined(sourceSummary.total ?? normalizedSummary.total),
		passed: numberOrUndefined(sourceSummary.passed ?? normalizedSummary.passed),
		failed: numberOrUndefined(sourceSummary.failed ?? normalizedSummary.failed),
		errored: numberOrUndefined(sourceSummary.errored ?? sourceSummary.error ?? normalizedSummary.errored),
		skipped: numberOrUndefined(sourceSummary.skipped ?? normalizedSummary.skipped),
		coverage: objectOrUndefined(coverageSummary),
	});
}

function normalizeObservationMetrics({ coverageSummary, hotspotSummary, normalizedResult } = {}) {
	const fuzzSummary = objectOrUndefined(normalizedResult?.summary) || {};
	return stripUndefined({
		coverage: objectOrUndefined(coverageSummary),
		db_query: objectOrUndefined(fuzzSummary.db_query_metrics),
		performance: objectOrUndefined(fuzzSummary.performance_metrics),
		admin_browser_errors: objectOrUndefined(fuzzSummary.admin_browser_errors),
		http_guardrail_outcomes: objectOrUndefined(fuzzSummary.http_guardrail_outcomes),
		hotspots: hotspotSummary ? stripUndefined({
			schema: hotspotSummary.schema,
			id: hotspotSummary.id,
			metric: hotspotSummary.metric,
			unit: hotspotSummary.unit,
			count: normalizeArray(hotspotSummary.items).length,
			items: hotspotSummary.items,
		}) : undefined,
	});
}

function requiredFuzzOutputFailures({ source = {}, context = {}, artifacts = [], normalizedResult, hotspotSummary } = {}) {
	const requirements = fuzzOutputRequirements(source, context);
	const failures = [];
	const missingMetricPaths = requirements.normalizedMetricPaths.filter((metricPath) => !hasNumericPath(normalizedResult, metricPath));
	if (missingMetricPaths.length > 0) {
		failures.push({
			severity: 'error',
			code: 'wp_codebox_fuzz_required_normalized_metrics_missing',
			message: 'WP Codebox fuzz result is missing declared normalized metric paths.',
			missing_metric_paths: missingMetricPaths,
		});
	}

	const missingArtifactKeys = requirements.artifactKeys.filter((key) => !artifacts.some((artifact) => fuzzArtifactMatchesKey(artifact, key)));
	if (missingArtifactKeys.length > 0) {
		failures.push({
			severity: 'error',
			code: 'wp_codebox_fuzz_required_output_artifacts_missing',
			message: 'WP Codebox fuzz result is missing declared output artifact keys.',
			missing_artifact_keys: missingArtifactKeys,
		});
	}

	const missingArtifactSchemas = requirements.artifactSchemas.filter((schema) => !artifacts.some((artifact) => fuzzArtifactMatchesSchema(artifact, schema)));
	if (missingArtifactSchemas.length > 0) {
		failures.push({
			severity: 'error',
			code: 'wp_codebox_fuzz_required_output_artifact_schemas_missing',
			message: 'WP Codebox fuzz result is missing declared output artifact schemas.',
			missing_artifact_schemas: missingArtifactSchemas,
		});
	}

	if (requirements.hotspotArtifactRequired && !hotspotSummary) {
		failures.push({
			severity: 'error',
			code: 'wp_codebox_fuzz_required_hotspot_artifact_missing',
			message: 'WP Codebox fuzz result is missing declared hotspot artifact payloads.',
		});
	}

	const missingEvidenceStatuses = requirements.evidenceStatuses.filter((requirement) => !pathMatchesExpectedStatus({ normalizedResult, source }, requirement));
	if (missingEvidenceStatuses.length > 0) {
		failures.push({
			severity: 'error',
			code: 'wp_codebox_fuzz_required_evidence_status_missing',
			message: 'WP Codebox fuzz result is missing declared evidence status checks.',
			missing_evidence_statuses: missingEvidenceStatuses,
		});
	}

	return failures;
}

function fuzzOutputRequirements(source = {}, context = {}) {
	const metadata = mergedFuzzRequestMetadata(source, context);
	const outputRequirements = objectOrUndefined(metadata.output_requirements || metadata.outputRequirements || metadata.required_outputs || metadata.requiredOutputs) || {};
	return {
		normalizedMetricPaths: normalizeStringList(
			metadata.required_normalized_metric_paths
			|| metadata.requiredNormalizedMetricPaths
			|| metadata.required_metric_paths
			|| metadata.requiredMetricPaths
			|| outputRequirements.required_normalized_metric_paths
			|| outputRequirements.requiredNormalizedMetricPaths
			|| outputRequirements.required_metric_paths
			|| outputRequirements.requiredMetricPaths
		),
		artifactKeys: normalizeStringList(
			metadata.required_artifact_keys
			|| metadata.requiredArtifactKeys
			|| outputRequirements.required_artifact_keys
			|| outputRequirements.requiredArtifactKeys
		),
		artifactSchemas: normalizeStringList(
			metadata.required_artifact_schemas
			|| metadata.requiredArtifactSchemas
			|| outputRequirements.required_artifact_schemas
			|| outputRequirements.requiredArtifactSchemas
		),
		evidenceStatuses: normalizeEvidenceStatusRequirements(
			metadata.required_evidence_statuses
			|| metadata.requiredEvidenceStatuses
			|| outputRequirements.required_evidence_statuses
			|| outputRequirements.requiredEvidenceStatuses
		),
		hotspotArtifactRequired: outputRequiresHotspotArtifact(metadata, outputRequirements),
	};
}

function outputRequiresHotspotArtifact(metadata = {}, outputRequirements = {}) {
	return normalizeStringList(
		metadata.required_artifact_keys
		|| metadata.requiredArtifactKeys
		|| outputRequirements.required_artifact_keys
		|| outputRequirements.requiredArtifactKeys
	).includes('fuzz.hotspot.summary')
		|| normalizeStringList(
			metadata.required_artifact_schemas
			|| metadata.requiredArtifactSchemas
			|| outputRequirements.required_artifact_schemas
			|| outputRequirements.requiredArtifactSchemas
		).includes(WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA);
}

function normalizeStringList(value) {
	return normalizeArray(value)
		.map((entry) => typeof entry === 'string' ? entry.trim() : '')
		.filter(Boolean);
}

function normalizeEvidenceStatusRequirements(value) {
	return normalizeArray(value)
		.map((entry) => {
			if (typeof entry === 'string') {
				const [evidencePath, status] = entry.split('=').map((part) => String(part || '').trim());
				return evidencePath && status ? { path: evidencePath, status } : undefined;
			}
			if (!objectOrUndefined(entry)) {
				return undefined;
			}
			const evidencePath = entry.path || entry.metric_path || entry.metricPath || entry.evidence_path || entry.evidencePath;
			const status = entry.status || entry.expected || entry.value;
			return evidencePath && status !== undefined ? { path: String(evidencePath), status: String(status) } : undefined;
		})
		.filter(Boolean);
}

function hasNumericPath(source, metricPath) {
	return valuesAtPath(source, metricPath).some((value) => Number.isFinite(Number(value)));
}

function pathMatchesExpectedStatus(sources = {}, requirement = {}) {
	return valuesAtPath(sources, requirement.path).some((value) => String(value) === String(requirement.status));
}

function valuesAtPath(source, valuePath) {
	if (!source || typeof valuePath !== 'string' || valuePath.trim() === '') {
		return [];
	}
	const normalizedPath = valuePath.trim().replace(/^\$\.?/, '').replace(/^\//, '').replace(/\//g, '.');
	const parts = normalizedPath.split('.').filter(Boolean);
	return valuesAtPathParts([source], parts);
}

function valuesAtPathParts(values, parts) {
	if (parts.length === 0) {
		return values;
	}
	const [part, ...rest] = parts;
	const next = [];
	for (const value of values) {
		if (part === '*') {
			if (Array.isArray(value)) {
				next.push(...value);
			} else if (objectOrUndefined(value)) {
				next.push(...Object.values(value));
			}
			continue;
		}
		if (Array.isArray(value) && /^\d+$/.test(part)) {
			next.push(value[Number(part)]);
			continue;
		}
		if (objectOrUndefined(value) && Object.prototype.hasOwnProperty.call(value, part)) {
			next.push(value[part]);
		}
	}
	return valuesAtPathParts(next.filter((value) => value !== undefined), rest);
}

function mergedFuzzRequestMetadata(source = {}, context = {}) {
	const input = context.request?.executor?.config?.runtime_task?.input || context.request?.input || context.input || {};
	return {
		...(objectOrUndefined(input.metadata) || {}),
		...(objectOrUndefined(source?.metadata) || {}),
	};
}

function wpCodeboxFuzzAllowsEmpty(source = {}, context = {}) {
	const input = context.request?.executor?.config?.runtime_task?.input || context.request?.input || context.input || {};
	const metadata = {
		...(objectOrUndefined(input.metadata) || {}),
		...(objectOrUndefined(source?.metadata) || {}),
	};
	const readiness = objectOrUndefined(metadata.readiness) || {};
	const genericPrimitive = objectOrUndefined(metadata.generic_primitive || metadata.genericPrimitive) || {};
	return metadata.allow_empty === true
		|| metadata.allowed_empty === true
		|| metadata.allowEmpty === true
		|| metadata.declared_only === true
		|| metadata.declaredOnly === true
		|| readiness.level === 'declared'
		|| readiness.declared_only === true
		|| readiness.declaredOnly === true
		|| genericPrimitive.status === 'blocked';
}

function wpCodeboxFuzzRequiredArtifactDeclarations(request = {}) {
	return normalizeArray(request.artifact_declarations || request.artifactDeclarations)
		.filter((artifact) => artifact?.required === true);
}

function fuzzArtifactMatchesKey(artifact = {}, key) {
	return [artifact.name, artifact.role, artifact.semantic_key || artifact.semanticKey, artifact.path, artifact.metadata?.semantic_key || artifact.metadata?.semanticKey, artifact.schema || artifact.metadata?.schema]
		.filter(Boolean)
		.map(String)
		.includes(String(key));
}

function fuzzArtifactMatchesSchema(artifact = {}, schema) {
	return [artifact.schema, artifact.artifact_schema, artifact.artifactSchema, artifact.metadata?.schema, artifact.payload?.schema, artifact.data?.schema, artifact.content?.schema]
		.filter(Boolean)
		.map(String)
		.includes(String(schema));
}
function wpCodeboxFuzzCaseCount(source = {}, normalizedResult) {
	const cases = source?.cases || source?.fuzz_cases || source?.fuzzCases || normalizedResult?.cases;
	if (Array.isArray(cases)) {
		return cases.length;
	}
	const total = source?.summary?.total ?? normalizedResult?.summary?.total;
	return Number.isFinite(Number(total)) ? Number(total) : 0;
}

function wpCodeboxFuzzExpectsCoverage(source = {}, context = {}, coverageSummary) {
	const input = context.request?.executor?.config?.runtime_task?.input || context.request?.input || context.input || {};
	const coverage = objectOrUndefined(input.metadata?.coverage || input.coverage || source?.coverage) || {};
	return normalizeArray(coverage.surface_ids || coverage.surfaceIds).length > 0
		|| normalizeArray(coverage.operations).length > 0
		|| Number(coverage.expected || coverage.discovered || coverageSummary?.surface_count) > 0;
}

function normalizeWpCodeboxFuzzResultSource(source = {}) {
	if (source?.schema === WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA) {
		return source;
	}
	const candidates = [
		source?.result,
		source?.agent_task_result?.result,
		source?.agentTaskResult?.result,
		source?.agent_result?.result,
		source?.agentResult?.result,
		source?.agent_task_result?.raw,
		source?.agentTaskResult?.raw,
		source?.outputs?.result,
		source?.outputs?.fuzz_result,
		source?.outputs?.fuzzResult,
	];
	return candidates.map((candidate) => findWpCodeboxFuzzSuiteResult(candidate)).find(Boolean) || source;
}

function findWpCodeboxFuzzSuiteResult(source, seen = new Set()) {
	if (!objectOrUndefined(source) || seen.has(source)) {
		return null;
	}
	if (source.schema === WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA) {
		return source;
	}
	seen.add(source);
	const candidates = [
		source.result,
		source.agent_task_result?.result,
		source.agentTaskResult?.result,
		source.agent_result?.result,
		source.agentResult?.result,
		source.raw,
		source.raw?.result,
		source.raw?.agent_runtime?.result,
		source.raw?.agentRuntime?.result,
		source.agent_task_result?.raw,
		source.agentTaskResult?.raw,
		source.outputs?.result,
		source.outputs?.fuzz_result,
		source.outputs?.fuzzResult,
	];
	for (const candidate of candidates) {
		const found = findWpCodeboxFuzzSuiteResult(candidate, seen);
		if (found) {
			return found;
		}
	}
	return null;
}

function normalizeWpCodeboxFuzzArtifacts(source = {}, result = {}) {
	const artifacts = [];
	appendFuzzArtifactRefs(artifacts, fuzzArtifactRefsFromSource(source, result));
	appendFuzzArtifactRefs(artifacts, fuzzArtifactRefsFromRuntimeCommands(source, result));
	appendFuzzArtifactRefs(artifacts, fuzzArtifactRefsFromEmbeddedWordPressResult(source));
	appendCaseArtifacts(artifacts, source?.cases || source?.fuzz_cases || source?.fuzzCases, 'fuzz_case');
	appendCaseArtifacts(artifacts, source?.wordpress_fuzz_result?.cases || source?.wordpressFuzzResult?.cases, 'fuzz_case');
	appendCaseArtifacts(artifacts, source?.failures || source?.errors || source?.failed_cases || source?.failedCases, 'failing_case');
	appendCaseArtifacts(artifacts, source?.repro_cases || source?.reproCases || source?.reproductions, 'repro_case');
	if (artifacts.length === 0) {
		appendStructuredFuzzArtifacts(artifacts, source);
	}
	appendInlineResultEnvelopeArtifact(artifacts, source);
	return dedupeArtifacts(artifacts.map(normalizeFuzzArtifact).filter(Boolean));
}

function appendInlineResultEnvelopeArtifact(artifacts, source = {}) {
	if (!objectOrUndefined(source) || !source.schema || hasFuzzArtifactRole(artifacts, 'result_envelope')) {
		return;
	}
	artifacts.push({
		name: 'result-envelope',
		role: 'result_envelope',
		semantic_key: 'fuzz.result.envelope',
		content: source,
		metadata: { schema: source.schema },
	});
}

function hasFuzzArtifactRole(artifacts = [], role) {
	return normalizeArray(artifacts).some((artifact) => {
		if (!objectOrUndefined(artifact)) {
			return false;
		}
		const identity = fuzzArtifactIdentity(artifact);
		return identity.role === role;
	});
}

function appendStructuredFuzzArtifacts(artifacts, source = {}) {
	const cases = normalizeArray(source?.cases || source?.fuzz_cases || source?.fuzzCases);
	if (cases.length === 0) {
		return;
	}
	if (source?.schema || source?.status) {
		artifacts.push({
			name: 'wp-codebox-fuzz-suite-result',
			role: 'fuzz_report',
			semantic_key: 'fuzz.result.normalized',
			content: source,
			metadata: { schema: source?.schema || WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA },
		});
	}
	if (cases.length > 0) {
		artifacts.push({
			name: 'case-log',
			role: 'case_log',
			semantic_key: 'fuzz.case.log',
			content: cases,
		});
		artifacts.push({
			name: 'replay-data',
			role: 'replay_data',
			semantic_key: 'fuzz.replay.data',
			content: cases.map((entry) => stripUndefined({ id: entry.id || entry.case_id || entry.caseId, target: objectOrUndefined(entry.target), input: objectOrUndefined(entry.metadata?.input || entry.input) })),
		});
	}
	const coverageSummary = normalizeCoverageSummary(source?.coverage_summary || source?.coverageSummary || source?.coverage?.summary);
	if (coverageSummary) {
		artifacts.push({
			name: 'coverage-summary',
			role: 'coverage_summary',
			semantic_key: 'fuzz.coverage.summary',
			content: coverageSummary,
		});
	}
}

function fuzzArtifactRefsFromSource(source = {}, result = {}) {
	return [
		source?.artifacts,
		source?.artifactRefs || source?.artifact_refs,
		result?.artifacts,
		result?.artifactRefs || result?.artifact_refs,
		{ fuzz_report: source?.fuzz_report || source?.fuzzReport || source?.report || source?.summary_report || source?.summaryReport },
		{ coverage: source?.coverage_artifact || source?.coverageArtifact || source?.wordpress_fuzz_coverage || source?.wordpressFuzzCoverage },
		{ normalized_fuzz_result: source?.wordpress_fuzz_result_artifact || source?.wordpressFuzzResultArtifact || source?.normalized_fuzz_result || source?.normalizedFuzzResult },
		{ hotspot_summary: source?.hotspot_summary_artifact || source?.hotspotSummaryArtifact || source?.hotspot_summary || source?.hotspotSummary },
		{ delete_boundary_artifact: source?.delete_boundary_artifacts || source?.deleteBoundaryArtifacts },
		{ sandbox_isolation_proof: source?.sandbox_isolation_proof || source?.sandboxIsolationProof || source?.sandbox_isolation_artifacts || source?.sandboxIsolationArtifacts },
		{ external_http_guardrail: source?.external_http_guardrail || source?.externalHttpGuardrail || source?.http_guardrail || source?.httpGuardrail },
		{ runtime_access: source?.runtime_access || source?.runtimeAccess || source?.runtime_access_artifact || source?.runtimeAccessArtifact },
	];
}

function fuzzArtifactRefsFromRuntimeCommands(...sources) {
	return sources.flatMap((source) => runtimeCommandArtifactSources(source));
}

function runtimeCommandArtifactSources(source, seen = new Set()) {
	if (!objectOrUndefined(source) || seen.has(source)) {
		return [];
	}
	seen.add(source);
	const commands = [
		...normalizeArray(source.runtime_commands || source.runtimeCommands),
		...normalizeArray(source.runtime_command_results || source.runtimeCommandResults),
		...normalizeArray(source.commands),
		...normalizeArray(source.steps),
	];
	return [
		source.runtime_command_artifacts,
		source.runtimeCommandArtifacts,
		source.runtime_artifacts,
		source.runtimeArtifacts,
		source.outputs?.artifacts,
		source.output?.artifacts,
		...commands.flatMap((command) => [command?.artifacts, command?.artifact_refs || command?.artifactRefs, command?.result?.artifacts, command?.outputs?.artifacts]),
		...commands.flatMap((command) => runtimeCommandArtifactSources(command, seen)),
	];
}

function fuzzArtifactRefsFromEmbeddedWordPressResult(source = {}) {
	return [
		source?.wordpress_fuzz_result?.artifacts || source?.wordpressFuzzResult?.artifacts,
		source?.wordpress_fuzz_result?.artifactRefs || source?.wordpress_fuzz_result?.artifact_refs || source?.wordpressFuzzResult?.artifactRefs || source?.wordpressFuzzResult?.artifact_refs,
	];
}

function appendFuzzArtifactRefs(artifacts, refs = []) {
	for (const ref of refs) {
		appendArtifactCandidates(artifacts, ref);
	}
}

function appendArtifactCandidates(artifacts, value) {
	if (Array.isArray(value)) {
		for (const artifact of value) {
			artifacts.push(artifact);
		}
		return;
	}
	if (!objectOrUndefined(value)) {
		return;
	}
	for (const [name, artifact] of Object.entries(value)) {
		appendNamedArtifact(artifacts, name, artifact);
	}
}

function appendNamedArtifact(artifacts, name, artifact) {
	if (artifact === undefined || artifact === null || artifact === '') {
		return;
	}
	if (Array.isArray(artifact)) {
		for (const item of artifact) {
			appendNamedArtifact(artifacts, name, item);
		}
		return;
	}
	artifacts.push(objectOrUndefined(artifact) ? { name, ...artifact } : { name, path: artifact });
}

function appendCaseArtifacts(artifacts, cases, fallbackRole) {
	for (const entry of normalizeArray(cases)) {
		const caseArtifacts = entry?.artifacts || entry?.artifactRefs || entry?.artifact_refs;
		if (caseArtifacts) {
			appendArtifactCandidates(artifacts, caseArtifacts);
		}
		if (entry?.repro || entry?.repro_case || entry?.reproCase) {
			appendNamedArtifact(artifacts, 'repro_case', entry.repro || entry.repro_case || entry.reproCase);
			continue;
		}
		if (entry?.artifact || entry?.path || entry?.url || entry?.file) {
			artifacts.push({ role: fallbackRole, ...entry });
		}
	}
}

function normalizeFuzzArtifact(artifact) {
	if (!objectOrUndefined(artifact)) {
		return null;
	}
	const identity = fuzzArtifactIdentity(artifact);
	const name = identity.name;
	const role = identity.role;
	if (!role) {
		return null;
	}
	const rawPath = artifact.path || artifact.file || artifact.artifact || artifact.uri;
	const rawUrl = artifact.url;
	const pathValue = reviewerSafeArtifactPath(rawPath);
	const urlValue = reviewerSafeArtifactUrl(rawUrl);
	const sha256 = artifact.sha256 || artifact.digest?.value;
	const artifactRef = artifact.ref || artifact.artifact_ref || artifact.artifactRef || artifact.semantic_ref || artifact.semanticRef || (artifact.semantic_key || artifact.semanticKey ? `artifact:${artifact.semantic_key || artifact.semanticKey}` : undefined);
	if (!hasConcreteArtifactReference({ ...artifact, path: pathValue, url: urlValue, ref: artifactRef, artifact_ref: artifactRef, sha256 })) {
		return null;
	}
	return stripUndefined({
		role,
		semantic_key: artifact.semantic_key || artifact.semanticKey || FUZZ_ARTIFACT_SEMANTIC_KEYS[role],
		name,
		path: pathValue,
		url: urlValue,
		artifact_ref: artifactRef,
		content_type: artifact.content_type || artifact.contentType || artifact.mime,
		sha256,
		size_bytes: numberOrUndefined(artifact.size_bytes ?? artifact.sizeBytes ?? artifact.bytes),
		payload: objectOrUndefined(artifact.payload),
		data: objectOrUndefined(artifact.data),
		content: objectOrUndefined(artifact.content),
		case_id: artifact.case_id || artifact.caseId,
		status: artifact.status,
		metadata: stripUndefined({
			...(objectOrUndefined(artifact.metadata) || {}),
			schema: artifact.schema || artifact.artifact_schema || artifact.artifactSchema || artifact.metadata?.schema,
			local_path_redacted: pathValue === undefined && isReviewerUnsafeArtifactPath(rawPath) ? true : undefined,
			local_url_redacted: urlValue === undefined && isReviewerUnsafeArtifactUrl(rawUrl) ? true : undefined,
		}),
	});
}

function reviewerSafeArtifactPath(value) {
	return isReviewerUnsafeArtifactPath(value) ? undefined : nonEmptyString(value);
}

function isReviewerUnsafeArtifactPath(value) {
	const text = nonEmptyString(value);
	return Boolean(text && (path.isAbsolute(text) || text.startsWith('file://') || /(^|\/)(Users|var\/folders)\//.test(text)));
}

function reviewerSafeArtifactUrl(value) {
	return isReviewerUnsafeArtifactUrl(value) ? undefined : nonEmptyString(value);
}

function isReviewerUnsafeArtifactUrl(value) {
	const text = nonEmptyString(value);
	return Boolean(text && /^(https?:\/\/)?(localhost|127\.0\.0\.1)([:/]|$)/i.test(text));
}

function normalizeDerivedFuzzArtifacts(source = {}, artifacts = []) {
	const derived = [];
	const coverageGapReports = [];
	const hotspotSummaries = [];
	for (const artifact of artifacts) {
		const payload = derivedArtifactPayload(artifact);
		const semanticKey = String(artifact.semantic_key || artifact.semanticKey || artifact.metadata?.semantic_key || artifact.metadata?.semanticKey || '').trim();
		const schema = String(artifact.schema || artifact.artifact_schema || artifact.artifactSchema || artifact.metadata?.schema || payload?.schema || '').trim();
		if (isCoverageGapArtifact({ semanticKey, schema, payload })) {
			const report = normalizeCoverageGapReportPayload(payload || artifact, artifact);
			if (report) {
				coverageGapReports.push(report);
				derived.push({ role: 'coverage_gap_report', semantic_key: semanticKey || 'fuzz.coverage.gap_report', schema, artifact });
			}
		}
		if (isHotspotSummaryArtifact({ semanticKey, schema, payload })) {
			const summary = normalizeFuzzHotspotSummary(payload || artifact, { provider: 'wp-codebox', taskId: source?.request_id || source?.requestId });
			if (summary) {
				hotspotSummaries.push(summary);
				derived.push({ role: 'hotspot_summary', semantic_key: semanticKey || 'fuzz.hotspot.summary', schema, artifact });
			}
		}
	}
	return stripUndefined({
		coverage_gap_reports: coverageGapReports.length > 0 ? coverageGapReports : undefined,
		hotspot_summary: hotspotSummaries.length > 0 ? normalizeFuzzHotspotSummary({ items: hotspotSummaries.flatMap((summary) => summary.items || []) }) : undefined,
		artifacts: derived,
	});
}

function validateWordPressFuzzPostprocessOutputs({ source = {}, context = {}, artifacts = [], derivedArtifacts = {}, hotspotSummary, normalizedResult } = {}) {
	if (!isProductionPostprocessRequired(source, context)) {
		return [];
	}
	const missing = requiredPostprocessOutputsMissing({ artifacts, derivedArtifacts, hotspotSummary, normalizedResult });
	if (missing.length === 0) {
		return [];
	}
	return [{
		severity: 'error',
		code: 'wp_codebox_fuzz_required_postprocess_outputs_missing',
		message: `Required WordPress fuzz postprocess outputs are missing: ${missing.join(', ')}.`,
		missing_outputs: missing,
	}];
}

function isProductionPostprocessRequired(source = {}, context = {}) {
	const metadata = objectOrUndefined(source.metadata) || {};
	const requestInput = context.request?.input || context.request?.inputs?.ability_input || context.request?.executor?.config?.runtime_task?.input || {};
	const inputMetadata = objectOrUndefined(requestInput.metadata) || {};
	return Boolean(
		metadata.production_campaign
		|| metadata.output_requirements?.production_campaign
		|| inputMetadata.production_campaign
		|| inputMetadata.output_requirements?.production_campaign
		|| inputMetadata.postprocess_binding?.required
		|| inputMetadata.postprocess_binding?.outputs?.some((artifact) => artifact.required)
	);
}

function requiredPostprocessOutputsMissing({ artifacts = [], derivedArtifacts = {}, hotspotSummary } = {}) {
	const checks = [
		['fuzz.coverage', hasArtifactSemanticKey(artifacts, 'fuzz.coverage')],
		['fuzz.hotspot.summary', Boolean(hotspotSummary?.items?.length > 0) || hasArtifactSemanticKey(artifacts, 'fuzz.hotspot.summary') || hasArtifactSchema(artifacts, WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA)],
		['fuzz.coverage.gap_report', hasArtifactSemanticKey(artifacts, 'fuzz.coverage.gap_report') || normalizeArray(derivedArtifacts.coverage_gap_reports).length > 0],
		['fuzz.hotspot.codebox', hasArtifactSemanticKey(artifacts, 'fuzz.hotspot.codebox') || hasArtifactSchema(artifacts, WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA) || hasArtifactName(artifacts, 'wordpress-hotspots')],
	];
	return checks.filter(([, present]) => !present).map(([key]) => key);
}

function hasArtifactSemanticKey(artifacts = [], semanticKey) {
	return normalizeArray(artifacts).some((artifact) => (artifact.semantic_key || artifact.semanticKey || artifact.metadata?.semantic_key || artifact.metadata?.semanticKey) === semanticKey);
}

function hasArtifactSchema(artifacts = [], schema) {
	return normalizeArray(artifacts).some((artifact) => (artifact.schema || artifact.artifact_schema || artifact.artifactSchema || artifact.metadata?.schema) === schema);
}

function hasArtifactName(artifacts = [], name) {
	return normalizeArray(artifacts).some((artifact) => artifact.name === name);
}

function derivedArtifactPayload(artifact = {}) {
	return objectOrUndefined(artifact.payload) || objectOrUndefined(artifact.data) || objectOrUndefined(artifact.content) || objectOrUndefined(artifact.result);
}

function isCoverageGapArtifact({ semanticKey = '', schema = '', payload } = {}) {
	const key = semanticKey.toLowerCase();
	const schemaName = schema.toLowerCase();
	return key === 'fuzz.coverage.gap_report'
		|| key === 'fuzz.coverage.gaps'
		|| key === 'coverage.gap_report'
		|| schemaName.includes('coverage-gap-report')
		|| (objectOrUndefined(payload) && (Array.isArray(payload.gaps) || Array.isArray(payload.coverage_gaps) || Array.isArray(payload.coverageGaps)) && (payload.expected !== undefined || payload.covered !== undefined || payload.status !== undefined));
}

function isHotspotSummaryArtifact({ semanticKey = '', schema = '', payload } = {}) {
	const key = semanticKey.toLowerCase();
	const schemaName = schema.toLowerCase();
	return key === 'fuzz.hotspot.summary'
		|| key === 'fuzz.hotspots'
		|| key === 'performance.hotspots'
		|| schemaName === WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA
		|| schemaName.includes('hotspot')
		|| (objectOrUndefined(payload) && (Array.isArray(payload.hotspots) || Array.isArray(payload.items)) && (payload.metric !== undefined || payload.ranking !== undefined));
}

function normalizeCoverageGapReportPayload(payload, artifact = {}) {
	if (!objectOrUndefined(payload)) {
		return undefined;
	}
	const gaps = normalizeArray(payload.coverage_gaps || payload.coverageGaps || payload.gaps).map((gap) => objectOrUndefined(gap) ? gap : { id: String(gap) });
	return stripUndefined({
		schema: 'homeboy/wordpress-fuzz-derived-coverage-gap-report/v1',
		status: payload.status,
		expected: numberOrUndefined(payload.expected),
		covered: numberOrUndefined(payload.covered),
		coverage_gaps: gaps.length > 0 ? gaps : undefined,
		artifact_ref: stripUndefined({
			name: artifact.name,
			path: artifact.path,
			semantic_key: artifact.semantic_key,
			schema: artifact.schema || artifact.metadata?.schema,
		}),
	});
}

function fuzzArtifactIdentity(artifact = {}) {
	const explicitRole = artifact.role || artifact.artifact_role || artifact.artifactRole;
	const explicitKind = artifact.kind || artifact.type;
	const semanticKey = artifact.semantic_key || artifact.semanticKey || artifact.metadata?.semantic_key || artifact.metadata?.semanticKey;
	const name = artifact.name || artifact.id || artifact.key || artifact.metadata?.artifactId || artifact.metadata?.id || explicitRole || explicitKind || semanticKey;
	const roleKind = ['typed-artifact', 'json'].includes(String(explicitKind || '').toLowerCase()) ? undefined : explicitKind;
	const semanticRole = FUZZ_ARTIFACT_ROLES_BY_SEMANTIC_KEY[semanticKey];
	return {
		name,
		role: normalizeFuzzArtifactRole(explicitRole || roleKind || semanticRole || name || semanticKey || artifact.path || artifact.url || artifact.file),
	};
}

function hasConcreteArtifactReference(artifact) {
	return Boolean(
		artifact.path
		|| artifact.url
		|| artifact.ref
		|| artifact.artifact_ref
		|| artifact.artifactRef
		|| artifact.sha256
		|| artifact.content
		|| artifact.value
		|| artifact.inline
	);
}

function normalizeFuzzArtifactRole(value) {
	const label = String(value || '').trim().toLowerCase().replace(/[\s.-]+/g, '_');
	if (!label) {
		return '';
	}
	if (['normalized_fuzz_result', 'wordpress_fuzz_result', 'normalized_result'].includes(label)) {
		return 'normalized_fuzz_result';
	}
	if (['result_envelope', 'fuzz_result_envelope'].includes(label)) {
		return 'result_envelope';
	}
	if (['case_log', 'case_logs', 'fuzz_case_log'].includes(label)) {
		return 'case_log';
	}
	if (['replay_data', 'replay_dataset', 'replay_inputs'].includes(label)) {
		return 'replay_data';
	}
	if (['coverage_gap_report', 'coverage_gaps', 'fuzz_coverage_gap_report', 'fuzz_coverage_gaps'].includes(label)) {
		return 'coverage_gap_report';
	}
	if (['coverage_summary', 'fuzz_coverage_summary'].includes(label)) {
		return 'coverage_summary';
	}
	if (['hotspot_summary', 'fuzz_hotspot_summary', 'hotspots', 'performance_hotspots'].includes(label)) {
		return 'hotspot_summary';
	}
	if (['sandbox_isolation_proof', 'sandbox_isolation', 'disposable_sandbox_proof', 'disposable_lifecycle', 'sandbox_lifecycle'].includes(label)) {
		return 'sandbox_isolation_proof';
	}
	if (['mutation_isolation', 'mutation_isolation_artifact', 'mutation_boundary', 'mutation_boundary_artifact'].includes(label)) {
		return 'mutation_isolation_artifact';
	}
	if (['delete_boundary', 'delete_boundary_artifact'].includes(label)) {
		return 'delete_boundary_artifact';
	}
	if (['external_http_guardrail', 'http_guardrail', 'homeboy_external_http', 'network_guardrail'].includes(label)) {
		return 'external_http_guardrail';
	}
	if (['runtime_access', 'runtime_access_artifact', 'runtime_profile_access', 'runtime_readiness'].includes(label)) {
		return 'runtime_access';
	}
	if (['observation_set', 'observations', 'measurements', 'fuzz_observation_set'].includes(label)) {
		return 'observation_set';
	}
	if (['coverage', 'wordpress_fuzz_coverage', 'fuzz_coverage', 'coverage_artifact'].includes(label)) {
		return 'coverage';
	}
	if (['fuzz_report', 'fuzz_run_report', 'report', 'summary', 'summary_report', 'result', 'results'].includes(label)) {
		return 'fuzz_report';
	}
	if (['failing_case', 'failed_case', 'failure', 'failures', 'error_case'].includes(label)) {
		return 'failing_case';
	}
	if (['repro_case', 'reproduction', 'repro', 'reproducer'].includes(label)) {
		return 'repro_case';
	}
	if (['replay', 'replay_case', 'replay_artifact'].includes(label)) {
		return 'repro_case';
	}
	if (['case_artifact', 'case_artifacts', 'artifact_ref', 'artifact_refs'].includes(label)) {
		return 'case_artifact';
	}
	if (['fuzz_case', 'case', 'cases'].includes(label)) {
		return 'fuzz_case';
	}
	if (/fail.*case|case.*fail/.test(label)) {
		return 'failing_case';
	}
	if (/repro/.test(label)) {
		return 'repro_case';
	}
	if (/replay/.test(label)) {
		return 'repro_case';
	}
	if (/case/.test(label)) {
		return 'case_artifact';
	}
	if (/coverage.*gap|gap.*coverage/.test(label)) {
		return 'coverage_gap_report';
	}
	if (/coverage/.test(label)) {
		return 'coverage';
	}
	if (/hotspot/.test(label)) {
		return 'hotspot_summary';
	}
	if (/external.*http|http.*guardrail|network.*guardrail/.test(label)) {
		return 'external_http_guardrail';
	}
	if (/runtime.*access|access.*runtime|runtime.*readiness/.test(label)) {
		return 'runtime_access';
	}
	if (/delete.*boundary|boundary.*delete/.test(label)) {
		return 'delete_boundary_artifact';
	}
	if (/report|summary|result/.test(label)) {
		return 'fuzz_report';
	}
	return '';
}

function normalizeEmbeddedWordPressFuzzResult(source = {}) {
	const candidate = source?.wordpress_fuzz_result || source?.wordpressFuzzResult || source?.normalized_result || source?.normalizedResult;
	if (objectOrUndefined(candidate)) {
		const artifacts = normalizeWpCodeboxFuzzArtifacts(candidate, source);
		return normalizeWordPressFuzzResult({
			...candidate,
			status: normalizeWordPressFuzzStatus(candidate.status),
			artifacts: artifacts.length > 0 ? artifacts : normalizeArray(candidate.artifacts),
		});
	}
	if (Array.isArray(source?.cases) || Array.isArray(source?.fuzz_cases) || Array.isArray(source?.fuzzCases)) {
		const artifacts = normalizeWpCodeboxFuzzArtifacts(source);
		return normalizeWordPressFuzzResult({
			schema: WORDPRESS_FUZZ_RESULT_SCHEMA,
			id: source.id || source.result_id || source.resultId || 'wp-codebox-wordpress-fuzz-result',
			plan_id: source.plan_id || source.planId || source.workload?.plan_id,
			status: normalizeWordPressFuzzStatus(source.status),
			started_at: source.started_at || source.startedAt,
			finished_at: source.finished_at || source.finishedAt,
			cases: source.cases || source.fuzz_cases || source.fuzzCases,
			artifacts,
			provenance: source.provenance || source.workload_manifest || source.workloadManifest,
			metadata: source.metadata,
		});
	}
	return undefined;
}

function normalizeWordPressFuzzStatus(value) {
	const status = String(value || '').toLowerCase();
	if (['success', 'succeeded', 'ok'].includes(status)) {
		return 'passed';
	}
	if (['failed', 'errored', 'partial', 'skipped', 'passed'].includes(status)) {
		return status;
	}
	return undefined;
}

function normalizeCoverageSummary(value) {
	if (!objectOrUndefined(value)) {
		return undefined;
	}
	return stripUndefined({
		surface_count: numberOrUndefined(value.surface_count ?? value.surfaceCount ?? value.surface ?? value.total),
		exercised_count: numberOrUndefined(value.exercised_count ?? value.exercisedCount ?? value.exercised ?? value.executed ?? value.generated),
		skipped_count: numberOrUndefined(value.skipped_count ?? value.skippedCount ?? value.skipped),
		failed_count: numberOrUndefined(value.failed_count ?? value.failedCount ?? value.failed),
		untested_count: numberOrUndefined(value.untested_count ?? value.untestedCount ?? value.untested),
		discovered_count: numberOrUndefined(value.discovered_count ?? value.discoveredCount ?? value.discovered),
		generated_count: numberOrUndefined(value.generated_count ?? value.generatedCount ?? value.generated),
		coverage_percent: numberOrUndefined(value.coverage_percent ?? value.coveragePercent),
	});
}

function normalizeCoverageGaps(value) {
	if (Array.isArray(value)) {
		return value;
	}
	return objectOrUndefined(value);
}

function numberOrUndefined(value) {
	const number = Number(value);
	return Number.isFinite(number) ? number : undefined;
}

function dedupeArtifacts(artifacts) {
	const seen = new Set();
	return artifacts.filter((artifact) => {
		const key = `${artifact.role}:${artifact.semantic_key || ''}:${artifact.path || ''}:${artifact.url || ''}:${artifact.name || ''}:${artifact.sha256 || ''}`;
		if (seen.has(key)) {
			return false;
		}
		seen.add(key);
		return true;
	});
}

function normalizeArray(value) {
	return Array.isArray(value) ? value.filter(Boolean) : [];
}

function objectOrUndefined(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

function requiredString(value, name) {
	if (typeof value !== 'string' || value.trim() === '') {
		throw new Error(`${name} is required.`);
	}
	return value;
}

function nonEmptyString(value) {
	return typeof value === 'string' && value.trim() !== '' ? value.trim() : undefined;
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
	DEFAULT_FUZZ_SUITE_ABILITY,
	DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY,
	DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA,
	REQUIRED_WP_CODEBOX_FUZZ_CONTRACT_PATHS,
	DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS,
	DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS,
	ARTIFACT_POSTPROCESS_COMMAND,
	ARTIFACT_POSTPROCESS_CONTRACT,
	FUZZ_ARTIFACT_SEMANTIC_KEYS,
	WORDPRESS_FUZZ_POSTPROCESS_BINDING_SCHEMA,
	WORDPRESS_FUZZ_POSTPROCESS_OUTPUTS,
	WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA,
	WP_CODEBOX_FUZZ_PREFLIGHT_SCHEMA,
	WORDPRESS_FUZZ_OBSERVATION_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_SCHEMA,
	WP_CODEBOX_FUZZ_EXECUTION_SCHEMA,
	WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
	buildWordPressFuzzCommandManifest,
	buildWordPressFuzzObservation,
	normalizeWpCodeboxFuzzArtifacts,
	normalizeWpCodeboxDestructiveReadiness,
	normalizeWpCodeboxFuzzSuiteResult,
	normalizeWordPressFuzzRuntimeTaskResult,
	wordpressFuzzPostprocessArtifactDeclarations,
	wordpressFuzzPostprocessBinding,
	wordpressFuzzPostprocessExpectedArtifacts,
	detectWpCodeboxPublicFuzzCapabilities,
	preflightWpCodeboxFuzzCapabilityContract,
	publicFuzzCliRunnerModeForRequest,
	runWpCodeboxFuzzSuite,
	runWpCodeboxPublicFuzzOperation,
	validateWpCodeboxRuntimeRequirementMounts,
	wpCodeboxFuzzExecutionRequest,
	wpCodeboxPublicCliInput,
	wpCodeboxFuzzSuiteAbility,
	wpCodeboxFuzzSuiteCommand,
	wpCodeboxFuzzSuiteResultSchema,
	wpCodeboxFuzzSuiteSchema,
	wpCodeboxFuzzRuntimeTaskRequest,
	wpCodeboxRuntimeContractManifest,
	wpCodeboxWordPressRuntimeContracts,
	wpCodeboxWordPressWorkloadRunAbility,
	wpCodeboxWordPressWorkloadRunCommand,
	wpCodeboxWordPressWorkloadRunInput,
	wpCodeboxWordPressWorkloadRunSchema,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzSuiteTaskRequest,
};
