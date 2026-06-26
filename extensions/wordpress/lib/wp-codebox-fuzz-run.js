'use strict';

/**
 * External dependencies
 */
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
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
	wpCodeboxCommand,
	wpCodeboxPluginStateStep,
} = require('./wp-codebox-recipe-helper');
const {
	resolveWpCodeboxIdentity,
	wpCodeboxIdentityMismatchDiagnostics,
} = require('./wp-codebox-resolver');
const {
	WP_CODEBOX_FUZZ_PUBLIC_ABILITIES,
	WP_CODEBOX_FUZZ_PUBLIC_COMMANDS,
	buildWordPressFuzzCommandManifest,
	requiredWpCodeboxContractsForFuzzPlan,
} = require('./wordpress-fuzz-command-manifest');

const WP_CODEBOX_FUZZ_SUITE_SCHEMA = 'wp-codebox/fuzz-suite/v1';
const WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA = 'wp-codebox/fuzz-suite-result/v1';
const WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA = 'wp-codebox/wordpress-hotspots/v1';
const WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA = 'homeboy/wordpress-codebox-fuzz-suite-consumer/v1';
const WP_CODEBOX_FUZZ_EXECUTION_SCHEMA = 'homeboy/wp-codebox-fuzz-execution/v1';
const WP_CODEBOX_FUZZ_PREFLIGHT_SCHEMA = 'homeboy/wp-codebox-fuzz-preflight/v1';
const WORDPRESS_FUZZ_OBSERVATION_SCHEMA = 'homeboy/wordpress-fuzz-observation/v1';
const DEFAULT_FUZZ_SUITE_ABILITY = 'wp-codebox/run-fuzz-suite';
const DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY = 'wp-codebox/run-wordpress-workload';
const DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA = 'wp-codebox/wordpress-workload-run/v1';
const DEFAULT_PUBLIC_CLI_MAX_BUFFER_BYTES = 1024 * 1024 * 128;
const DEFAULT_WP_CODEBOX_PUBLIC_CLI_BIN = 'wp';
const WP_CODEBOX_PUBLIC_CLI_COMMANDS = WP_CODEBOX_FUZZ_PUBLIC_COMMANDS;
const ARTIFACT_POSTPROCESS_COMMAND = 'homeboy.artifact-postprocess';
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
	result_envelope: 'fuzz.result.envelope',
	normalized_fuzz_result: 'fuzz.result.normalized',
	coverage: 'fuzz.coverage',
};

const FALLBACK_WP_CODEBOX_RUNTIME_CONTRACT_MANIFEST = {
	schema: 'wp-codebox/runtime-contract-manifest/v1',
	version: 1,
	schemas: {
		wordpressRuntime: {
			workloadRun: DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA,
			fuzzSuite: WP_CODEBOX_FUZZ_SUITE_SCHEMA,
			fuzzSuiteResult: WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
		},
	},
	abilities: {
		wordpressRuntime: {
			runWorkload: DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY,
			runFuzzSuite: DEFAULT_FUZZ_SUITE_ABILITY,
		},
	},
};

function wpCodeboxRuntimeContractManifest(options = {}) {
	const manifest = options.runtimeContractManifest || options.runtime_contract_manifest || options.manifest || options.contractManifest || options.contract_manifest;
	return objectOrUndefined(manifest) ? manifest : FALLBACK_WP_CODEBOX_RUNTIME_CONTRACT_MANIFEST;
}

function wpCodeboxWordPressRuntimeContracts(options = {}) {
	const manifest = wpCodeboxRuntimeContractManifest(options);
	return {
		manifest,
		abilities: objectOrUndefined(manifest.abilities?.wordpressRuntime) || FALLBACK_WP_CODEBOX_RUNTIME_CONTRACT_MANIFEST.abilities.wordpressRuntime,
		schemas: objectOrUndefined(manifest.schemas?.wordpressRuntime) || FALLBACK_WP_CODEBOX_RUNTIME_CONTRACT_MANIFEST.schemas.wordpressRuntime,
	};
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

function wpCodeboxFuzzSuiteInput(options = {}) {
	const source = normalizeHomeboyFuzzWorkloadSource(options);
	const cases = normalizeWpCodeboxFuzzSuiteCases(source || options);
	const artifacts = source?.artifacts || options.artifacts;
	const postprocessBinding = options.postprocessBinding || options.postprocess_binding || source?.postprocess_binding || source?.postprocessBinding;
	return stripUndefined({
		schema: wpCodeboxFuzzSuiteSchema(options),
		id: options.id || options.runId || options.run_id,
		goal: options.goal || options.instructions,
		version: options.version,
		target: options.target,
		cases,
		steps: normalizeWordPressWorkloadSteps(source?.steps || options.steps),
		metadata: stripUndefined({
			...(objectOrUndefined(options.metadata) || {}),
			workload: objectOrUndefined(options.workload),
			seeds: normalizeArray(options.seeds).length > 0 ? normalizeArray(options.seeds) : undefined,
			limits: objectOrUndefined(options.limits),
			coverage: objectOrUndefined(options.coverage),
			runtime_profile: objectOrUndefined(options.runtimeProfile || options.runtime_profile),
			artifacts: objectOrUndefined(artifacts),
			postprocess_binding: objectOrUndefined(postprocessBinding),
		}),
	});
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

function normalizeWpCodeboxFuzzSuiteCases(source = {}) {
	const directCases = normalizeArray(source.cases);
	if (source.schema !== HOMEBOY_FUZZ_WORKLOAD_SCHEMA) {
		return directCases;
	}
	const planCases = homeboyFuzzWorkloadPlanCases(source);
	if (planCases.length > 0) {
		return planCases.map((entry, index) => homeboyFuzzWorkloadPlanCaseToWpCodeboxCase(entry, source, index));
	}
	return directCases.map((entry, index) => homeboyFuzzWorkloadCaseToWpCodeboxCase(entry, source, index));
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
	const command = homeboyFuzzPlanCaseRuntimeCommand(entry);
	const input = homeboyFuzzPlanCaseRuntimeInput(entry, manifest, caseId);
	return stripUndefined({
		id: caseId,
		case_id: caseId,
		target: { kind: 'runtime', id: command, entrypoint: command },
		description: entry.description || manifest.label,
		input,
		inputs: objectOrUndefined(entry.inputs),
		phases: homeboyFuzzWorkloadPlanCasePhases(entry, manifest, artifacts),
		artifacts,
		metadata: stripUndefined({
			...(objectOrUndefined(entry.metadata) || {}),
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

function homeboyFuzzRuntimeCommandArgs(input) {
	return Array.isArray(input?.args) ? input.args : homeboyFuzzCommandArgs(input);
}

function homeboyFuzzPlanCaseRuntimeCommand(entry = {}) {
	return entry.command || entry.target?.entrypoint || entry.target?.id || 'wordpress.run-fuzz-case';
}

function homeboyFuzzPlanCaseRuntimeInput(entry = {}, manifest = {}, caseId) {
	if (objectOrUndefined(entry.input)) {
		return homeboyFuzzRuntimeCommandInput(entry.input);
	}
	return stripUndefined({
		case_id: caseId,
		target_id: entry.target_id,
		surface_id: entry.surface_id,
		intent: entry.intent,
		operation_id: entry.operation_id || entry.operationId,
		operation: objectOrUndefined(entry.operation),
		seed: entry.seed || manifest.seed,
		skip_reasons: nonEmptyArray(entry.skip_reasons || entry.skipReasons),
		destructive_reasons: nonEmptyArray(entry.destructive_reasons || entry.destructiveReasons),
	});
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

function homeboyFuzzWorkloadCaseToWpCodeboxCase(entry = {}, manifest = {}, index = 0) {
	const caseId = entry.case_id || entry.caseId || entry.id || `${manifest.id || 'fuzz-workload'}:${index}`;
	const intent = objectOrUndefined(entry.intent) || {};
	const execute = objectOrUndefined(intent.execute) || {};
	const artifacts = normalizeHomeboyFuzzCaseArtifacts(entry, manifest);
	const command = homeboyFuzzWorkloadGenericPrimitiveCommand(manifest) || 'wordpress.run-workload';
	const input = homeboyFuzzWorkloadRuntimeCommandInput(entry, manifest, execute);
	return stripUndefined({
		id: caseId,
		case_id: caseId,
		target: { kind: 'runtime', id: command, entrypoint: command },
		description: entry.description || manifest.label,
		input,
		inputs: objectOrUndefined(entry.inputs),
		phases: homeboyFuzzWorkloadCasePhases(entry, manifest, intent, artifacts),
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
	if (Array.isArray(direct.args)) {
		return direct;
	}
	const args = homeboyFuzzCommandArgs(direct);
	return args.length > 0 ? { args } : direct;
}

function homeboyFuzzWorkloadRuntimeCommandInput(entry = {}, manifest = {}, execute = {}) {
	if (objectOrUndefined(entry.input)) {
		return homeboyFuzzRuntimeCommandInput(entry.input);
	}
	const workloadDefinition = objectOrUndefined(execute.definition) || objectOrUndefined(manifest.workload?.definition);
	if (workloadDefinition) {
		return homeboyFuzzWorkloadRunInputFromDefinition(workloadDefinition, { entry: execute.entry || manifest.workload?.entry });
	}
	const workloadPath = execute.path || manifest.workload?.path;
	if (typeof workloadPath === 'string' && workloadPath.trim() !== '') {
		if (String(execute.type || manifest.workload?.type || '').toLowerCase() === 'php') {
			return wpCodeboxWordPressWorkloadRunInput({
				id: execute.entry || manifest.workload?.entry,
				steps: [{ command: 'wordpress.run-workload', args: [`path=${workloadPath}`, 'type=php'] }],
				metadata: stripUndefined({
					source_path: workloadPath,
					source_entry: execute.entry || manifest.workload?.entry,
					source_type: 'php',
				}),
			});
		}
		const workloadInput = homeboyFuzzWorkloadRunInputFromFile(workloadPath, { entry: execute.entry || manifest.workload?.entry });
		if (workloadInput) {
			return workloadInput;
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
		before: source.before,
		steps,
		after: source.after,
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
		before: source.before,
		steps,
		after: source.after,
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

function homeboyFuzzWorkloadCasePhases(entry = {}, manifest = {}, intent = {}, artifacts = []) {
	if (objectOrUndefined(entry.phases)) {
		return entry.phases;
	}
	const execute = objectOrUndefined(intent.execute) || {};
	const activation = intent.plugin?.activation;
	const workloadPath = execute.path || manifest.workload?.path;
	const workloadDefinition = objectOrUndefined(execute.definition) || objectOrUndefined(manifest.workload?.definition);
	const genericCommand = homeboyFuzzWorkloadGenericPrimitiveCommand(manifest);
	const setup = typeof activation === 'string' && activation.trim() !== ''
		? [wpCodeboxPluginActivationStep(activation)]
		: undefined;
	const action = homeboyFuzzWorkloadCaseAction({ genericCommand, workloadPath, workloadDefinition, execute });
	const collect = normalizeArray(intent.collect).length > 0 ? normalizeArray(intent.collect) : artifacts.map((artifact) => ({ artifact: artifact.name }));
	const assert = collect
		.map((item) => item?.artifact)
		.filter(Boolean)
		.map((artifact) => ({ command: 'wordpress.collect-workload-result', args: [`artifact=${artifact}`] }));
	return stripUndefined({ setup, action, assert: assert.length > 0 ? assert : undefined });
}

function wpCodeboxPluginActivationStep(plugin) {
	return wpCodeboxPluginStateStep({ activate: [plugin] });
}

function homeboyFuzzWorkloadCaseAction({ genericCommand, workloadPath, workloadDefinition, execute = {} } = {}) {
	if (typeof genericCommand === 'string') {
		return [{ command: genericCommand, args: homeboyFuzzCommandArgs(objectOrUndefined(execute.parameters) || {}) }];
	}
	if (typeof workloadPath === 'string' && workloadPath.trim() !== '') {
		return [{ command: 'wordpress.run-workload', args: [`path=${workloadPath}`] }];
	}
	if (objectOrUndefined(workloadDefinition)) {
		return [{ command: 'wordpress.run-workload' }];
	}
	return [];
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
		backend: options.backend || 'codebox',
		runtime: options.runtime || options.runtimeId || options.runtime_id || 'wp-codebox',
		taskId: requiredString(options.taskId || options.task_id, 'taskId'),
		ability: options.ability || wpCodeboxFuzzSuiteAbility(options),
		abilityInput: input,
		artifactDeclarations: options.artifactDeclarations || options.artifact_declarations || DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS,
		expectedArtifacts: options.expectedArtifacts || options.expected_artifacts || DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS,
		goal: options.goal || options.instructions || 'Delegate WordPress fuzz execution to WP Codebox and return the declared fuzz artifacts.',
		instructions: options.instructions || 'Delegate WordPress fuzz execution to WP Codebox and return the declared fuzz artifacts.',
	});
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
	const runtimeRequirements = options.runtimeRequirements || options.runtime_requirements;
	return stripUndefined({
		schema: WP_CODEBOX_FUZZ_EXECUTION_SCHEMA,
		task_id: taskId,
		ability,
		command: wpCodeboxCommandFromFuzzAbility(ability, options),
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
	return stripUndefined({
		schema: wpCodeboxWordPressWorkloadRunSchema(options),
		id: options.id || options.runId || options.run_id,
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
		metadata: objectOrUndefined(options.metadata),
	});
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
			contract: 'homeboy/artifact-postprocess/v1',
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

	const cliResult = await runWpCodeboxPublicCli(command, wpCodeboxPublicCliInput(request, options), options);
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
		return { ...fuzzCase, input: { ...workload, mounts, staged_files: stagedFiles, stagedFiles: undefined } };
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
	for (const plugin of [...normalizeArray(runtimeRequirements.extra_plugins), ...normalizeArray(runtimeRequirements.component_contracts)]) {
		const slug = objectOrUndefined(plugin)?.slug;
		if (typeof slug === 'string' && slug.trim()) {
			return slug.trim();
		}
	}
	return undefined;
}

function detectWpCodeboxPublicFuzzCapabilities(options = {}) {
	if (options.publicCliCapabilities || options.public_cli_capabilities) {
		return normalizeWpCodeboxPublicFuzzCapabilities(options.publicCliCapabilities || options.public_cli_capabilities);
	}
	if (options.capabilities?.public_cli || options.capabilities?.publicCli) {
		return normalizeWpCodeboxPublicFuzzCapabilities(options.capabilities.public_cli || options.capabilities.publicCli);
	}

	const commands = {};
	for (const command of WP_CODEBOX_PUBLIC_CLI_COMMANDS) {
		const result = runWpCodeboxPublicCliHelp(command, options);
		commands[command] = result.status === 0;
	}
	return normalizeWpCodeboxPublicFuzzCapabilities({ commands });
}

function preflightWpCodeboxFuzzCapabilityContract(options = {}) {
	const request = options.request || options.taskRequest || options.task_request || null;
	const wpCodeboxIdentity = resolveWpCodeboxIdentity(options);
	const identityDiagnostics = wpCodeboxIdentityMismatchDiagnostics(wpCodeboxIdentity);
	const capabilities = detectWpCodeboxPublicFuzzCapabilities(options);
	const manifest = wpCodeboxRuntimeContractManifest(options);
	const wordpressRuntimeAbilities = objectOrUndefined(manifest.abilities?.wordpressRuntime) || {};
	const commandManifest = buildWordPressFuzzCommandManifest();
	const suiteInput = wpCodeboxFuzzRequestInput(request, options);
	const plan = suiteInput.homeboy_fuzz_workload?.plan || suiteInput.homeboyFuzzWorkload?.plan || suiteInput.metadata?.workload?.plan || request?.input?.plan || options.plan || {};
	const requiredPlanContracts = requiredWpCodeboxContractsForFuzzPlan(plan);
	const requiredAbilities = { ...WP_CODEBOX_FUZZ_PUBLIC_ABILITIES };
	const requiredCommands = requiredPublicCommandsForRequest(request, requiredPlanContracts);
	const missingContracts = [];
	for (const diagnostic of identityDiagnostics) {
		missingContracts.push({
			type: 'identity_mismatch',
			message: diagnostic.message,
			diagnostic,
		});
	}

	for (const command of requiredCommands) {
		if (capabilities.commands?.[command] !== true) {
			missingContracts.push({
				type: 'public_cli_command',
				command,
				message: `WP Codebox public CLI must expose \`${command}\` before Homeboy dispatches WordPress fuzz workloads.`,
			});
		}
	}

	if (wordpressRuntimeAbilities.runFuzzSuite !== requiredAbilities.runFuzzSuite) {
		missingContracts.push({
			type: 'ability',
			ability: requiredAbilities.runFuzzSuite,
			message: `WP Codebox runtime contract must declare \`${requiredAbilities.runFuzzSuite}\` for fuzz-suite dispatch.`,
		});
	}
	if (wordpressRuntimeAbilities.runWorkload !== requiredAbilities.runWorkload) {
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
		wp_codebox_identity: wpCodeboxIdentity,
		required: {
			commands: requiredCommands,
			abilities: requiredAbilities,
			capabilities: requiredPlanContracts.capabilities,
		},
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
		return ['run-wordpress-workload'];
	}
	if (ability === DEFAULT_FUZZ_SUITE_ABILITY) {
		const input = wpCodeboxFuzzRequestInput(request);
		return unique([
			'run-fuzz-suite',
			...normalizeArray(requiredPlanContracts.commands),
			...(suiteInputRequiresWorkloadCommand(input) ? ['run-wordpress-workload'] : []),
		]);
	}
	return requiredPlanContracts.commands?.length > 0 ? unique(requiredPlanContracts.commands) : [...WP_CODEBOX_PUBLIC_CLI_COMMANDS];
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

function normalizeWpCodeboxPublicFuzzCapabilities(input = {}) {
	const commands = objectOrUndefined(input.commands) || input;
	return {
		schema: 'homeboy/wp-codebox-public-fuzz-capabilities/v1',
		commands: Object.fromEntries(WP_CODEBOX_PUBLIC_CLI_COMMANDS.map((command) => [command, commands[command] === true])),
	};
}

function publicFuzzCliCommandForRequest(request = {}, capabilities = {}) {
	if (request.command && capabilities.commands?.[request.command]) {
		return request.command;
	}
	const ability = wpCodeboxFuzzRequestAbility(request);
	if (ability === DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY && capabilities.commands?.['run-wordpress-workload']) {
		return 'run-wordpress-workload';
	}
	if (capabilities.commands?.['run-fuzz-suite']) {
		return 'run-fuzz-suite';
	}
	if (capabilities.commands?.['run-wordpress-workload']) {
		return 'run-wordpress-workload';
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

function runWpCodeboxPublicCliHelp(command, options = {}) {
	return runWpCodeboxPublicCliCommand([command, '--help'], options);
}

async function runWpCodeboxPublicCli(command, input, options = {}) {
	const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-'));
	const inputFile = path.join(tempDir, 'input.json');
	try {
		fs.writeFileSync(inputFile, `${JSON.stringify(input)}\n`, 'utf8');
		return runWpCodeboxPublicCliCommand([command, '--input-file', inputFile, '--format=json'], options);
	} finally {
		fs.rmSync(tempDir, { recursive: true, force: true });
	}
}

function runWpCodeboxPublicCliCommand(args, options = {}) {
	if (typeof options.runPublicCli === 'function') {
		return normalizeCliResult(options.runPublicCli({ command: wpCodeboxPublicCliBin(options), args, stdin: options.stdin }, options));
	}
	if (typeof options.runCli === 'function') {
		return normalizeCliResult(options.runCli({ command: wpCodeboxPublicCliBin(options), args, stdin: options.stdin }, options));
	}

	const invocation = wpCodeboxPublicCliInvocation(options);
	const result = spawnSync(invocation.command, [...invocation.args, ...args], {
		input: options.stdin,
		encoding: 'utf8',
		env: { ...process.env, ...(options.env || {}) },
		cwd: options.cwd,
		maxBuffer: options.maxBuffer || options.max_buffer || DEFAULT_PUBLIC_CLI_MAX_BUFFER_BYTES,
	});
	return normalizeCliResult(result);
}

function wpCodeboxCommandFromFuzzAbility(ability, options = {}) {
	if (ability === wpCodeboxWordPressWorkloadRunAbility(options) || ability === DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY) {
		return 'run-wordpress-workload';
	}
	return 'run-fuzz-suite';
}

function wpCodeboxPublicCliBin(options = {}) {
	if (options.wpCliBin || options.wp_cli_bin) {
		return options.wpCliBin || options.wp_cli_bin;
	}
	const env = { ...process.env, ...(options.env || {}) };
	if (env.wpCliBin || env.wp_cli_bin || env.HOMEBOY_WP_CLI_BIN || env.WP_CLI_BIN) {
		return env.wpCliBin || env.wp_cli_bin || env.HOMEBOY_WP_CLI_BIN || env.WP_CLI_BIN;
	}
	const identity = resolveWpCodeboxIdentity(options);
	return identity.selectionSource === 'default' ? DEFAULT_WP_CODEBOX_PUBLIC_CLI_BIN : identity.bin;
}

function wpCodeboxPublicCliInvocation(options = {}) {
	const identity = resolveWpCodeboxIdentity(options);
	const bin = wpCodeboxPublicCliBin(options);
	if (bin === identity.bin) {
		return identity.invocation;
	}
	const invocation = wpCodeboxCommand(bin);
	const executable = path.basename(String(bin || '')).toLowerCase();
	const usesWpCliNamespace = executable === 'wp' || executable === 'wp-cli' || executable === 'wp-cli.phar';
	return {
		command: invocation.command,
		args: usesWpCliNamespace ? [...invocation.args, 'codebox'] : invocation.args,
	};
}

function normalizeCliResult(result = {}) {
	let status = 0;
	if (Number.isInteger(result.status)) {
		status = result.status;
	} else if (result.error) {
		status = 1;
	}
	return {
		status,
		stdout: String(result.stdout || ''),
		stderr: String(result.stderr || result.error?.message || ''),
	};
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
			...wordpressFuzzResultCaseObservations(source?.wordpress_fuzz_result || source?.wordpressFuzzResult || source?.normalized_result || source?.normalizedResult),
		],
		metadata: stripUndefined({
			wp_codebox_result_schema: source?.schema,
		}),
	}, { provider: 'wp-codebox', taskId: source?.request_id || source?.requestId || context.request?.task_id });
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
	failures.push(...validateWordPressFuzzPostprocessOutputs({ source, context, artifacts, derivedArtifacts, hotspotSummary, normalizedResult }));

	return failures;
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
	const pathValue = artifact.path || artifact.file || artifact.artifact || artifact.uri;
	const sha256 = artifact.sha256 || artifact.digest?.value;
	if (!hasConcreteArtifactReference({ ...artifact, path: pathValue, sha256 })) {
		return null;
	}
	return stripUndefined({
		role,
		semantic_key: artifact.semantic_key || artifact.semanticKey || FUZZ_ARTIFACT_SEMANTIC_KEYS[role],
		name,
		path: pathValue,
		url: artifact.url,
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
		}),
	});
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
	const name = artifact.name || artifact.id || artifact.key || explicitRole || explicitKind;
	return {
		name,
		role: normalizeFuzzArtifactRole(explicitRole || explicitKind || name || artifact.path || artifact.url || artifact.file),
	};
}

function hasConcreteArtifactReference(artifact) {
	return Boolean(
		artifact.path
		|| artifact.url
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
	FALLBACK_WP_CODEBOX_RUNTIME_CONTRACT_MANIFEST,
	DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS,
	DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS,
	ARTIFACT_POSTPROCESS_COMMAND,
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
	normalizeWpCodeboxFuzzSuiteResult,
	normalizeWordPressFuzzRuntimeTaskResult,
	wordpressFuzzPostprocessArtifactDeclarations,
	wordpressFuzzPostprocessBinding,
	wordpressFuzzPostprocessExpectedArtifacts,
	detectWpCodeboxPublicFuzzCapabilities,
	preflightWpCodeboxFuzzCapabilityContract,
	runWpCodeboxFuzzSuite,
	runWpCodeboxPublicFuzzOperation,
	wpCodeboxFuzzExecutionRequest,
	wpCodeboxFuzzSuiteAbility,
	wpCodeboxFuzzSuiteResultSchema,
	wpCodeboxFuzzSuiteSchema,
	wpCodeboxFuzzRuntimeTaskRequest,
	wpCodeboxRuntimeContractManifest,
	wpCodeboxWordPressRuntimeContracts,
	wpCodeboxWordPressWorkloadRunAbility,
	wpCodeboxWordPressWorkloadRunInput,
	wpCodeboxWordPressWorkloadRunSchema,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzSuiteTaskRequest,
};
