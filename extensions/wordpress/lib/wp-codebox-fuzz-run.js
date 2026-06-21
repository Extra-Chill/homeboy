'use strict';

/**
 * Internal dependencies
 */
const {
	wordpressRuntimeTaskRequest,
} = require('./wordpress-runtime-task-planner');

const WP_CODEBOX_FUZZ_RUN_SCHEMA = 'wp-codebox/fuzz-run/v1';
const WP_CODEBOX_FUZZ_RUN_RESULT_SCHEMA = 'wp-codebox/fuzz-run-result/v1';
const WORDPRESS_CODEBOX_FUZZ_RUN_CONSUMER_SCHEMA = 'homeboy/wordpress-codebox-fuzz-run-consumer/v1';
const DEFAULT_FUZZ_RUN_ABILITY = 'wp-codebox/fuzz-run';
const DEFAULT_FUZZ_RUN_EXPECTED_ARTIFACTS = [
	'wp-codebox-fuzz-run-result',
	'wordpress-fuzz-coverage',
];

function wpCodeboxFuzzRunInput(options = {}) {
	return stripUndefined({
		schema: WP_CODEBOX_FUZZ_RUN_SCHEMA,
		id: options.id || options.runId || options.run_id,
		target: options.target,
		workload: options.workload,
		cases: normalizeArray(options.cases),
		seeds: normalizeArray(options.seeds),
		limits: objectOrUndefined(options.limits),
		coverage: objectOrUndefined(options.coverage),
		runtime_profile: objectOrUndefined(options.runtimeProfile || options.runtime_profile),
		artifacts: objectOrUndefined(options.artifacts),
		metadata: objectOrUndefined(options.metadata),
	});
}

function wpCodeboxFuzzRunTaskRequest(options = {}) {
	const input = wpCodeboxFuzzRunInput(options.input || options.abilityInput || options.ability_input || options);
	return wordpressRuntimeTaskRequest({
		...options,
		taskId: requiredString(options.taskId || options.task_id, 'taskId'),
		ability: options.ability || DEFAULT_FUZZ_RUN_ABILITY,
		abilityInput: input,
		expectedArtifacts: options.expectedArtifacts || options.expected_artifacts || DEFAULT_FUZZ_RUN_EXPECTED_ARTIFACTS,
		instructions: options.instructions || 'Delegate WordPress fuzz execution to WP Codebox and return the declared fuzz artifacts.',
	});
}

async function runWpCodeboxFuzzRun(options = {}) {
	const request = wpCodeboxFuzzRunTaskRequest(options);
	const runner = options.runFuzzRun || options.runRuntimeTask || options.runTask;
	if (typeof runner !== 'function') {
		throw new Error('runWpCodeboxFuzzRun requires runFuzzRun, runRuntimeTask, or runTask.');
	}

	const result = await runner(request, options);
	return normalizeWpCodeboxFuzzRunResult(result, { request });
}

function normalizeWpCodeboxFuzzRunResult(result = {}, context = {}) {
	const source = result?.json || result?.result || result?.output || result;
	const status = source?.status || source?.outcome?.status || result?.status || '';
	return stripUndefined({
		schema: WORDPRESS_CODEBOX_FUZZ_RUN_CONSUMER_SCHEMA,
		delegated_schema: WP_CODEBOX_FUZZ_RUN_SCHEMA,
		result_schema: source?.schema || result?.schema || WP_CODEBOX_FUZZ_RUN_RESULT_SCHEMA,
		request_id: source?.request_id || source?.requestId || context.request?.task_id,
		status,
		succeeded: status ? ['succeeded', 'success', 'passed', 'ok'].includes(String(status).toLowerCase()) : undefined,
		coverage: source?.coverage,
		artifacts: source?.artifacts || result?.artifacts,
		failures: normalizeArray(source?.failures || source?.errors),
		metadata: objectOrUndefined(source?.metadata),
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
	DEFAULT_FUZZ_RUN_EXPECTED_ARTIFACTS,
	WORDPRESS_CODEBOX_FUZZ_RUN_CONSUMER_SCHEMA,
	WP_CODEBOX_FUZZ_RUN_RESULT_SCHEMA,
	WP_CODEBOX_FUZZ_RUN_SCHEMA,
	normalizeWpCodeboxFuzzRunResult,
	runWpCodeboxFuzzRun,
	wpCodeboxFuzzRunInput,
	wpCodeboxFuzzRunTaskRequest,
};
