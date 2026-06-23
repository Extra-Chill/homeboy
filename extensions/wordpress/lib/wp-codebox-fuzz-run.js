'use strict';

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

const WP_CODEBOX_FUZZ_SUITE_SCHEMA = 'wp-codebox/fuzz-suite/v1';
const WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA = 'wp-codebox/fuzz-suite-result/v1';
const WP_CODEBOX_FUZZ_RUN_SCHEMA = legacyWpCodeboxFuzzRunSchemaAlias();
const WP_CODEBOX_FUZZ_RUN_RESULT_SCHEMA = legacyWpCodeboxFuzzRunResultSchemaAlias();
const WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA = 'homeboy/wordpress-codebox-fuzz-suite-consumer/v1';
const WORDPRESS_CODEBOX_FUZZ_RUN_CONSUMER_SCHEMA = legacyWordPressCodeboxFuzzRunConsumerSchemaAlias();
const DEFAULT_FUZZ_SUITE_ABILITY = 'wp-codebox/run-fuzz-suite';
const DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY = 'wp-codebox/run-wordpress-workload';
const DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA = 'wp-codebox/wordpress-workload-run/v1';
const DEFAULT_FUZZ_RUN_ABILITY = legacyWpCodeboxFuzzRunAbilityAlias();
const DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS = [
	'wp-codebox-fuzz-suite-result',
	'wordpress-fuzz-coverage',
	'result-envelope',
	'case-log',
	'replay-data',
	'coverage-summary',
];
const DEFAULT_FUZZ_RUN_EXPECTED_ARTIFACTS = legacyWpCodeboxFuzzRunExpectedArtifactsAlias();
const DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS = [
	{
		name: 'wp-codebox-fuzz-suite-result',
		semantic_key: 'fuzz.result.normalized',
		content_type: 'application/json',
		required: true,
	},
	{
		name: 'wordpress-fuzz-coverage',
		semantic_key: 'fuzz.coverage',
		content_type: 'application/json',
		required: true,
	},
	{
		name: 'fuzz-report',
		semantic_key: 'fuzz.report',
		content_type: 'application/json',
		required: false,
	},
	{
		name: 'fuzz-case-artifacts',
		semantic_key: 'fuzz.case.artifact',
		content_type: 'application/json',
		required: false,
	},
	{
		name: 'fuzz-repro-cases',
		semantic_key: 'fuzz.case.repro',
		required: false,
	},
	{
		name: 'result-envelope',
		semantic_key: 'fuzz.result.envelope',
		content_type: 'application/json',
		required: true,
	},
	{
		name: 'case-log',
		semantic_key: 'fuzz.case.log',
		content_type: 'application/jsonl',
		required: true,
	},
	{
		name: 'replay-data',
		semantic_key: 'fuzz.replay.data',
		content_type: 'application/json',
		required: true,
	},
	{
		name: 'coverage-summary',
		semantic_key: 'fuzz.coverage.summary',
		content_type: 'application/json',
		required: true,
	},
];
const DEFAULT_FUZZ_RUN_ARTIFACT_DECLARATIONS = legacyWpCodeboxFuzzRunArtifactDeclarationsAlias();
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
	return stripUndefined({
		schema: wpCodeboxFuzzSuiteSchema(options),
		id: options.id || options.runId || options.run_id,
		goal: options.goal || options.instructions,
		version: options.version,
		target: options.target,
		cases,
		metadata: stripUndefined({
			...(objectOrUndefined(options.metadata) || {}),
			workload: objectOrUndefined(options.workload),
			seeds: normalizeArray(options.seeds).length > 0 ? normalizeArray(options.seeds) : undefined,
			limits: objectOrUndefined(options.limits),
			coverage: objectOrUndefined(options.coverage),
			runtime_profile: objectOrUndefined(options.runtimeProfile || options.runtime_profile),
			artifacts: objectOrUndefined(artifacts),
		}),
	});
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
	const command = entry.command || entry.target?.entrypoint || entry.target?.id;
	return stripUndefined({
		id: caseId,
		case_id: caseId,
		target: { kind: 'runtime', id: command, entrypoint: command },
		description: entry.description || manifest.label,
		input: objectOrUndefined(entry.input),
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
		? [{ command: 'wordpress.ensure-plugin-active', args: [`plugin=${activation}`] }]
		: undefined;
	const command = entry.command || entry.target?.entrypoint || entry.target?.id;
	const action = typeof command === 'string' && command.trim() !== ''
		? [{ command, args: homeboyFuzzCommandArgs(entry.input) }]
		: [];
	const assert = artifacts
		.map((artifact) => artifact.name)
		.filter(Boolean)
		.map((artifact) => ({ command: 'wordpress.collect-workload-result', args: [`artifact=${artifact}`] }));
	return stripUndefined({ setup, action, assert: assert.length > 0 ? assert : undefined });
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
	return stripUndefined({
		id: caseId,
		case_id: caseId,
		target: { kind: 'runtime', id: command, entrypoint: command },
		description: entry.description || manifest.label,
		input: stripUndefined({
			path: execute.path || manifest.workload?.path,
			type: execute.type || manifest.workload?.type,
			entry: execute.entry || manifest.workload?.entry,
			parameters: objectOrUndefined(execute.parameters),
		}),
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

function homeboyFuzzWorkloadCasePhases(entry = {}, manifest = {}, intent = {}, artifacts = []) {
	if (objectOrUndefined(entry.phases)) {
		return entry.phases;
	}
	const execute = objectOrUndefined(intent.execute) || {};
	const activation = intent.plugin?.activation;
	const path = execute.path || manifest.workload?.path;
	const genericCommand = homeboyFuzzWorkloadGenericPrimitiveCommand(manifest);
	const setup = typeof activation === 'string' && activation.trim() !== ''
		? [{ command: 'wordpress.ensure-plugin-active', args: [`plugin=${activation}`] }]
		: undefined;
	const action = homeboyFuzzWorkloadCaseAction({ genericCommand, path, execute });
	const collect = normalizeArray(intent.collect).length > 0 ? normalizeArray(intent.collect) : artifacts.map((artifact) => ({ artifact: artifact.name }));
	const assert = collect
		.map((item) => item?.artifact)
		.filter(Boolean)
		.map((artifact) => ({ command: 'wordpress.collect-workload-result', args: [`artifact=${artifact}`] }));
	return stripUndefined({ setup, action, assert: assert.length > 0 ? assert : undefined });
}

function homeboyFuzzWorkloadCaseAction({ genericCommand, path, execute = {} } = {}) {
	if (typeof genericCommand === 'string') {
		return [{ command: genericCommand, args: homeboyFuzzCommandArgs(objectOrUndefined(execute.parameters) || {}) }];
	}
	if (typeof path === 'string' && path.trim() !== '') {
		return [{ command: 'wordpress.run-workload', args: [`path=${path}`] }];
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

function wpCodeboxFuzzRunInput(options = {}) {
	return wpCodeboxFuzzSuiteInput(options);
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
		steps: normalizeArray(options.steps),
		after: normalizeArray(options.after),
		metadata: objectOrUndefined(options.metadata),
	});
}

function wpCodeboxFuzzRunTaskRequest(options = {}) {
	return wpCodeboxFuzzSuiteTaskRequest(options);
}

async function runWpCodeboxFuzzSuite(options = {}) {
	const request = wpCodeboxFuzzSuiteTaskRequest(options);
	const runner = options.runFuzzSuite || options.runFuzzRun || options.runRuntimeTask || options.runTask;
	if (typeof runner !== 'function') {
		throw new Error('runWpCodeboxFuzzSuite requires runFuzzSuite, runFuzzRun, runRuntimeTask, or runTask.');
	}

	const result = await runner(request, options);
	return normalizeWpCodeboxFuzzSuiteResult(result, { request });
}

function runWpCodeboxFuzzRun(options = {}) {
	return runWpCodeboxFuzzSuite(options);
}

function normalizeWpCodeboxFuzzSuiteResult(result = {}, context = {}) {
	const source = normalizeWpCodeboxFuzzResultSource(result?.json || result?.result || result?.output || result);
	let status = source?.status || source?.outcome?.status || result?.status || '';
	const artifacts = normalizeWpCodeboxFuzzArtifacts(source, result);
	const coverageSummary = normalizeCoverageSummary(source?.coverage_summary || source?.coverageSummary || source?.coverage?.summary);
	const coverageGaps = normalizeCoverageGaps(source?.coverage_gaps || source?.coverageGaps || source?.coverage?.gaps);
	const normalizedResult = normalizeEmbeddedWordPressFuzzResult(source);
	const contractFailures = wpCodeboxFuzzContractFailures({ source, result, context, artifacts, coverageSummary, normalizedResult });
	if (contractFailures.length > 0 && ['succeeded', 'success', 'passed', 'ok'].includes(String(status).toLowerCase())) {
		status = 'failed';
	}
	return stripUndefined({
		schema: WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA,
		delegated_schema: WP_CODEBOX_FUZZ_SUITE_SCHEMA,
		result_schema: source?.schema || result?.schema || WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
		request_id: source?.request_id || source?.requestId || context.request?.task_id,
		status,
		succeeded: status ? ['succeeded', 'success', 'passed', 'ok'].includes(String(status).toLowerCase()) : undefined,
		coverage: source?.coverage,
		coverage_summary: coverageSummary,
		coverage_gaps: coverageGaps,
		wordpress_fuzz_result: normalizedResult,
		artifacts,
		failures: [...normalizeArray(source?.failures || source?.errors || source?.diagnostics), ...contractFailures],
		metadata: stripUndefined({
			...(objectOrUndefined(source?.metadata) || {}),
			suite: objectOrUndefined(source?.suite),
			summary: objectOrUndefined(source?.summary),
		}),
	});
}

function wpCodeboxFuzzContractFailures({ source = {}, result = {}, context = {}, artifacts = [], coverageSummary, normalizedResult }) {
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

	return failures;
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

function normalizeWpCodeboxFuzzRunResult(result = {}, context = {}) {
	return normalizeWpCodeboxFuzzSuiteResult(result, context);
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

function legacyWpCodeboxFuzzRunSchemaAlias() {
	return WP_CODEBOX_FUZZ_SUITE_SCHEMA;
}

function legacyWpCodeboxFuzzRunResultSchemaAlias() {
	return WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA;
}

function legacyWordPressCodeboxFuzzRunConsumerSchemaAlias() {
	return WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA;
}

function legacyWpCodeboxFuzzRunAbilityAlias() {
	return DEFAULT_FUZZ_SUITE_ABILITY;
}

function legacyWpCodeboxFuzzRunExpectedArtifactsAlias() {
	return DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS;
}

function legacyWpCodeboxFuzzRunArtifactDeclarationsAlias() {
	return DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS;
}

function normalizeWpCodeboxFuzzArtifacts(source = {}, result = {}) {
	const artifacts = [];
	appendFuzzArtifactRefs(artifacts, fuzzArtifactRefsFromSource(source, result));
	appendFuzzArtifactRefs(artifacts, fuzzArtifactRefsFromEmbeddedWordPressResult(source));
	appendCaseArtifacts(artifacts, source?.cases || source?.fuzz_cases || source?.fuzzCases, 'fuzz_case');
	appendCaseArtifacts(artifacts, source?.wordpress_fuzz_result?.cases || source?.wordpressFuzzResult?.cases, 'fuzz_case');
	appendCaseArtifacts(artifacts, source?.failures || source?.errors || source?.failed_cases || source?.failedCases, 'failing_case');
	appendCaseArtifacts(artifacts, source?.repro_cases || source?.reproCases || source?.reproductions, 'repro_case');
	return dedupeArtifacts(artifacts.map(normalizeFuzzArtifact).filter(Boolean));
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
	if (['coverage_summary', 'fuzz_coverage_summary'].includes(label)) {
		return 'coverage_summary';
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
	if (/coverage/.test(label)) {
		return 'coverage';
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
		exercised_count: numberOrUndefined(value.exercised_count ?? value.exercisedCount ?? value.exercised),
		skipped_count: numberOrUndefined(value.skipped_count ?? value.skippedCount ?? value.skipped),
		failed_count: numberOrUndefined(value.failed_count ?? value.failedCount ?? value.failed),
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
	DEFAULT_FUZZ_RUN_ABILITY,
	DEFAULT_FUZZ_RUN_ARTIFACT_DECLARATIONS,
	DEFAULT_FUZZ_RUN_EXPECTED_ARTIFACTS,
	DEFAULT_FUZZ_SUITE_ABILITY,
	DEFAULT_WORDPRESS_WORKLOAD_RUN_ABILITY,
	DEFAULT_WORDPRESS_WORKLOAD_RUN_SCHEMA,
	FALLBACK_WP_CODEBOX_RUNTIME_CONTRACT_MANIFEST,
	DEFAULT_FUZZ_SUITE_ARTIFACT_DECLARATIONS,
	DEFAULT_FUZZ_SUITE_EXPECTED_ARTIFACTS,
	FUZZ_ARTIFACT_SEMANTIC_KEYS,
	WORDPRESS_CODEBOX_FUZZ_RUN_CONSUMER_SCHEMA,
	WORDPRESS_CODEBOX_FUZZ_SUITE_CONSUMER_SCHEMA,
	WP_CODEBOX_FUZZ_RUN_RESULT_SCHEMA,
	WP_CODEBOX_FUZZ_RUN_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_SCHEMA,
	legacyWpCodeboxFuzzRunAbilityAlias,
	legacyWpCodeboxFuzzRunArtifactDeclarationsAlias,
	legacyWpCodeboxFuzzRunExpectedArtifactsAlias,
	legacyWpCodeboxFuzzRunResultSchemaAlias,
	legacyWpCodeboxFuzzRunSchemaAlias,
	legacyWordPressCodeboxFuzzRunConsumerSchemaAlias,
	normalizeWpCodeboxFuzzArtifacts,
	normalizeWpCodeboxFuzzRunResult,
	normalizeWpCodeboxFuzzSuiteResult,
	runWpCodeboxFuzzRun,
	runWpCodeboxFuzzSuite,
	wpCodeboxFuzzSuiteAbility,
	wpCodeboxFuzzSuiteResultSchema,
	wpCodeboxFuzzSuiteSchema,
	wpCodeboxFuzzRunInput,
	wpCodeboxFuzzRunTaskRequest,
	wpCodeboxRuntimeContractManifest,
	wpCodeboxWordPressRuntimeContracts,
	wpCodeboxWordPressWorkloadRunAbility,
	wpCodeboxWordPressWorkloadRunInput,
	wpCodeboxWordPressWorkloadRunSchema,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzSuiteTaskRequest,
};
