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
const WP_CODEBOX_FUZZ_RUN_SCHEMA = WP_CODEBOX_FUZZ_SUITE_SCHEMA;
const WP_CODEBOX_FUZZ_RUN_RESULT_SCHEMA = WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA;
const WORDPRESS_CODEBOX_FUZZ_RUN_CONSUMER_SCHEMA = 'homeboy/wordpress-codebox-fuzz-run-consumer/v1';
const DEFAULT_FUZZ_SUITE_ABILITY = 'wp-codebox/fuzz-suite';
const DEFAULT_FUZZ_RUN_ABILITY = DEFAULT_FUZZ_SUITE_ABILITY;
const DEFAULT_FUZZ_RUN_EXPECTED_ARTIFACTS = [
	'wp-codebox-fuzz-suite-result',
	'wordpress-fuzz-coverage',
];
const DEFAULT_FUZZ_RUN_ARTIFACT_DECLARATIONS = [
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
];

const FUZZ_ARTIFACT_SEMANTIC_KEYS = {
	fuzz_report: 'fuzz.report',
	fuzz_case: 'fuzz.case',
	failing_case: 'fuzz.case.failing',
	case_artifact: 'fuzz.case.artifact',
	repro_case: 'fuzz.case.repro',
	normalized_fuzz_result: 'fuzz.result.normalized',
	coverage: 'fuzz.coverage',
};

function wpCodeboxFuzzRunInput(options = {}) {
	return stripUndefined({
		schema: WP_CODEBOX_FUZZ_SUITE_SCHEMA,
		id: options.id || options.runId || options.run_id,
		version: options.version,
		target: options.target,
		cases: normalizeArray(options.cases),
		metadata: stripUndefined({
			...(objectOrUndefined(options.metadata) || {}),
			workload: objectOrUndefined(options.workload),
			seeds: normalizeArray(options.seeds).length > 0 ? normalizeArray(options.seeds) : undefined,
			limits: objectOrUndefined(options.limits),
			coverage: objectOrUndefined(options.coverage),
			runtime_profile: objectOrUndefined(options.runtimeProfile || options.runtime_profile),
			artifacts: objectOrUndefined(options.artifacts),
		}),
	});
}

const wpCodeboxFuzzSuiteInput = wpCodeboxFuzzRunInput;

function wpCodeboxFuzzRunTaskRequest(options = {}) {
	const input = wpCodeboxFuzzRunInput(options.input || options.abilityInput || options.ability_input || options);
	return wordpressRuntimeTaskRequest({
		...options,
		taskId: requiredString(options.taskId || options.task_id, 'taskId'),
		ability: options.ability || DEFAULT_FUZZ_RUN_ABILITY,
		abilityInput: input,
		artifactDeclarations: options.artifactDeclarations || options.artifact_declarations || DEFAULT_FUZZ_RUN_ARTIFACT_DECLARATIONS,
		expectedArtifacts: options.expectedArtifacts || options.expected_artifacts || DEFAULT_FUZZ_RUN_EXPECTED_ARTIFACTS,
		instructions: options.instructions || 'Delegate WordPress fuzz execution to WP Codebox and return the declared fuzz artifacts.',
	});
}

const wpCodeboxFuzzSuiteTaskRequest = wpCodeboxFuzzRunTaskRequest;

async function runWpCodeboxFuzzRun(options = {}) {
	const request = wpCodeboxFuzzRunTaskRequest(options);
	const runner = options.runFuzzRun || options.runRuntimeTask || options.runTask;
	if (typeof runner !== 'function') {
		throw new Error('runWpCodeboxFuzzRun requires runFuzzRun, runRuntimeTask, or runTask.');
	}

	const result = await runner(request, options);
	return normalizeWpCodeboxFuzzRunResult(result, { request });
}

const runWpCodeboxFuzzSuite = runWpCodeboxFuzzRun;

function normalizeWpCodeboxFuzzRunResult(result = {}, context = {}) {
	const source = result?.json || result?.result || result?.output || result;
	const status = source?.status || source?.outcome?.status || result?.status || '';
	const artifacts = normalizeWpCodeboxFuzzArtifacts(source, result);
	const coverageSummary = normalizeCoverageSummary(source?.coverage_summary || source?.coverageSummary || source?.coverage?.summary);
	const coverageGaps = normalizeCoverageGaps(source?.coverage_gaps || source?.coverageGaps || source?.coverage?.gaps);
	const normalizedResult = normalizeEmbeddedWordPressFuzzResult(source);
	return stripUndefined({
		schema: WORDPRESS_CODEBOX_FUZZ_RUN_CONSUMER_SCHEMA,
		delegated_schema: WP_CODEBOX_FUZZ_RUN_SCHEMA,
		result_schema: source?.schema || result?.schema || WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
		request_id: source?.request_id || source?.requestId || context.request?.task_id,
		status,
		succeeded: status ? ['succeeded', 'success', 'passed', 'ok'].includes(String(status).toLowerCase()) : undefined,
		coverage: source?.coverage,
		coverage_summary: coverageSummary,
		coverage_gaps: coverageGaps,
		wordpress_fuzz_result: normalizedResult,
		artifacts,
		failures: normalizeArray(source?.failures || source?.errors || source?.diagnostics),
		metadata: stripUndefined({
			...(objectOrUndefined(source?.metadata) || {}),
			suite: objectOrUndefined(source?.suite),
			summary: objectOrUndefined(source?.summary),
		}),
	});
}

const normalizeWpCodeboxFuzzSuiteResult = normalizeWpCodeboxFuzzRunResult;

function normalizeWpCodeboxFuzzArtifacts(source = {}, result = {}) {
	const artifacts = [];
	appendArtifactCandidates(artifacts, source?.artifacts);
	appendArtifactCandidates(artifacts, source?.artifactRefs || source?.artifact_refs);
	appendArtifactCandidates(artifacts, result?.artifacts);
	appendArtifactCandidates(artifacts, result?.artifactRefs || result?.artifact_refs);
	appendNamedArtifact(artifacts, 'fuzz_report', source?.fuzz_report || source?.fuzzReport || source?.report || source?.summary_report || source?.summaryReport);
	appendNamedArtifact(artifacts, 'coverage', source?.coverage_artifact || source?.coverageArtifact || source?.wordpress_fuzz_coverage || source?.wordpressFuzzCoverage);
	appendNamedArtifact(artifacts, 'normalized_fuzz_result', source?.wordpress_fuzz_result_artifact || source?.wordpressFuzzResultArtifact || source?.normalized_fuzz_result || source?.normalizedFuzzResult);
	appendCaseArtifacts(artifacts, source?.cases || source?.fuzz_cases || source?.fuzzCases, 'fuzz_case');
	appendCaseArtifacts(artifacts, source?.failures || source?.errors || source?.failed_cases || source?.failedCases, 'failing_case');
	appendCaseArtifacts(artifacts, source?.repro_cases || source?.reproCases || source?.reproductions, 'repro_case');
	return dedupeArtifacts(artifacts.map(normalizeFuzzArtifact).filter(Boolean));
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
	const name = artifact.name || artifact.id || artifact.key || artifact.role || artifact.type || artifact.kind;
	const role = normalizeFuzzArtifactRole(artifact.role || artifact.artifact_role || artifact.artifactRole || name || artifact.path || artifact.url || artifact.file);
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
		case_id: artifact.case_id || artifact.caseId,
		status: artifact.status,
		metadata: stripUndefined({
			...(objectOrUndefined(artifact.metadata) || {}),
		}),
	});
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
	FUZZ_ARTIFACT_SEMANTIC_KEYS,
	WORDPRESS_CODEBOX_FUZZ_RUN_CONSUMER_SCHEMA,
	WP_CODEBOX_FUZZ_RUN_RESULT_SCHEMA,
	WP_CODEBOX_FUZZ_RUN_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_RESULT_SCHEMA,
	WP_CODEBOX_FUZZ_SUITE_SCHEMA,
	normalizeWpCodeboxFuzzArtifacts,
	normalizeWpCodeboxFuzzRunResult,
	normalizeWpCodeboxFuzzSuiteResult,
	runWpCodeboxFuzzRun,
	runWpCodeboxFuzzSuite,
	wpCodeboxFuzzRunInput,
	wpCodeboxFuzzRunTaskRequest,
	wpCodeboxFuzzSuiteInput,
	wpCodeboxFuzzSuiteTaskRequest,
};
