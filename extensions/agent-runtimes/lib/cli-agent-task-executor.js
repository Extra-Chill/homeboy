'use strict';

/**
 * External dependencies
 */
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
	AGENT_TASK_REQUEST_SCHEMA,
} = require('../../agent-task-contracts/agent-task-provider-contract');
const {
	normalizeAgentTaskOutcome,
} = require('../../runtime-agent-ci/lib/agent-task-outcome-normalizer');

const DEFAULT_MAX_BUFFER = 10 * 1024 * 1024;

/**
 * Build a CLI agent-task executor from a per-provider configuration.
 *
 * The four standalone CLI runtimes (codex, claude-code, opencode, pi) share a
 * single execution spine: validate the AgentTaskRequest, optionally run a
 * provider preflight, resolve the command, spawn it synchronously, and map the
 * spawn result onto a normalized AgentTaskOutcome. Every genuine per-provider
 * delta is expressed through the `spec` passed here, so the shared spine stays
 * the single source of truth for control flow while each runtime keeps its own
 * explicit configuration.
 *
 * @param {Object} spec Provider configuration.
 * @return {{execute: Function, outcome: Function, validationFailure: Function}} Executor surface.
 */
function createCliAgentTaskExecutor(spec) {
	const {
		backend,
		runtime = backend,
		providerId,
		providerLabel,
		defaultSummary,
		validateRuntime = true,
		requireConfig = true,
		emitArtifacts = true,
		secretEnv = [],
		artifactProvider = backend,
		collectArtifacts = false,
		timeoutFallback = (config) => config.timeout_seconds,
		resolveCommandSpec,
		buildArgs,
		buildSpawn,
		preflight,
		onEmptyCommand,
		finalizeOutcome,
		messages = {},
		invalidCommandOutcome,
		notFoundOutcome,
		spawnErrorOutcome,
		successOutcome,
		failureOutcome,
	} = spec;

	const buildInvalidCommand = invalidCommandOutcome || ((commandSpec) => ({
		status: 'provider_error',
		failure_classification: commandSpec.classification || 'provider',
		failure_code: commandSpec.code || messages.invalidCommand.code,
		summary: commandSpec.summary || messages.invalidCommand.summary,
		diagnostics: [{ classification: 'provider_setup', message: commandSpec.error }],
	}));

	const buildNotFound = notFoundOutcome || (() => ({
		status: 'provider_error',
		failure_classification: 'provider',
		failure_code: messages.notFound.code,
		summary: messages.notFound.summary,
		diagnostics: [{ classification: 'provider_setup', message: messages.notFound.hint }],
	}));

	const buildSpawnError = spawnErrorOutcome || ((context, timedOut) => ({
		status: timedOut ? 'timeout' : 'provider_error',
		failure_classification: timedOut ? 'timeout' : 'provider',
		failure_code: timedOut ? messages.timeout.code : messages.spawnFailed.code,
		summary: timedOut ? messages.timeout.summary : messages.spawnFailed.summary,
		diagnostics: [{ classification: timedOut ? 'timeout' : 'provider_setup', message: context.spawnResult.error.message }],
	}));

	const buildSuccess = successOutcome || (() => ({
		status: 'succeeded',
		summary: messages.success.summary,
		diagnostics: [{ classification: 'provider', message: messages.success.diag }],
		metadata: { exit_code: 0 },
	}));

	const buildFailure = failureOutcome || ((context) => ({
		status: 'failed',
		failure_classification: 'execution_failed',
		failure_code: messages.failed.code,
		summary: messages.failed.summary,
		diagnostics: [{ classification: 'execution_failed', message: messages.failed.diag(context.spawnResult.status) }],
		metadata: {
			exit_code: context.spawnResult.status,
			...(context.spawnResult.signal ? { signal: context.spawnResult.signal } : {}),
		},
	}));

	function outcomeRequest(request = {}) {
		return request.task_id ? request : { ...request, task_id: 'unknown-task' };
	}

	function outcome(request = {}, values = {}) {
		const normalizedRequest = outcomeRequest(request);
		const normalized = normalizeAgentTaskOutcome(normalizedRequest, values, {
			provider: providerId,
			providerLabel,
			status: values.status || 'provider_error',
			failureClassification: values.failure_classification,
			failureCode: values.failure_code,
			summary: values.summary || defaultSummary,
			artifacts: emitArtifacts ? (values.artifacts || []) : [],
			evidenceRefs: emitArtifacts ? (values.evidence_refs || []) : [],
			metadata: values.metadata || {},
		});
		return finalizeOutcome ? finalizeOutcome(normalizedRequest, normalized) : normalized;
	}

	function validationFailure(request, message) {
		return outcome(request, {
			status: 'provider_error',
			failure_classification: 'invalid_input',
			failure_code: messages.invalidRequest.code,
			summary: messages.invalidRequest.summary,
			diagnostics: [{ classification: 'request_validation', message }],
		});
	}

	function validateRequest(request) {
		if (!request || typeof request !== 'object' || Array.isArray(request)) {
			return 'Request must be a JSON object.';
		}
		if (request.schema !== AGENT_TASK_REQUEST_SCHEMA) {
			return `Request schema must be ${AGENT_TASK_REQUEST_SCHEMA}.`;
		}
		if (!request.task_id || typeof request.task_id !== 'string') {
			return 'Request task_id is required.';
		}
		if (request.executor?.backend !== backend) {
			return `Request executor.backend must be ${backend}.`;
		}
		if (validateRuntime && request.executor?.runtime !== undefined && request.executor.runtime !== runtime) {
			return `Request executor.runtime must be ${runtime}.`;
		}
		if (requireConfig && (!request.executor.config || typeof request.executor.config !== 'object' || Array.isArray(request.executor.config))) {
			return 'Request executor.config is required.';
		}
		if (!request.instructions || typeof request.instructions !== 'string') {
			return 'Request instructions are required.';
		}
		return null;
	}

	function processArtifacts(request, config, spawnResult) {
		const artifactDir = config.artifacts_path || config.artifactsPath || request.artifacts_path || process.env.HOMEBOY_AGENT_TASK_ARTIFACTS_DIR || process.env.HOMEBOY_RUNTIME_AGENT_ARTIFACTS_DIR || '';
		if (!artifactDir) {
			return {};
		}
		const artifacts = [];
		const evidence_refs = [];
		for (const stream of ['stdout', 'stderr']) {
			const content = redactSecrets(String(spawnResult[stream] || ''), secretEnv);
			if (!content) {
				continue;
			}
			const filePath = path.join(artifactDir, `${safeFileSegment(request.task_id)}-${artifactProvider}-${stream}.txt`);
			fs.mkdirSync(path.dirname(filePath), { recursive: true });
			fs.writeFileSync(filePath, content);
			const artifact = { id: `${artifactProvider}-${stream}`, name: `${artifactProvider}-${stream}`, kind: 'provider-process-stream', stream, path: filePath, bytes: Buffer.byteLength(content) };
			artifacts.push(artifact);
			evidence_refs.push({ kind: 'provider-process-stream', label: `${artifactProvider} ${stream}`, path: filePath });
		}
		return { artifacts, evidence_refs };
	}

	function execute(request = {}, options = {}) {
		const validationError = validateRequest(request);
		if (validationError) {
			return validationFailure(request, validationError);
		}

		if (preflight) {
			const preflightOutcome = preflight(request, options);
			if (preflightOutcome) {
				return outcome(request, preflightOutcome);
			}
		}

		const config = request.executor.config || {};
		const commandSpec = resolveCommandSpec(config, options);
		if (commandSpec.error) {
			return outcome(request, buildInvalidCommand(commandSpec));
		}

		const cwd = resolveCwd(request, config);
		if (onEmptyCommand && !commandSpec.command) {
			return outcome(request, onEmptyCommand({ request, config, commandSpec, cwd }));
		}

		const args = buildArgs(request, config, commandSpec);
		const timeoutSeconds = timeoutSecondsFromLimits(request.limits, timeoutFallback(config));
		const spawnExtra = buildSpawn(request, config, options);
		const spawnResult = spawnSync(commandSpec.command, args, {
			cwd,
			encoding: 'utf8',
			maxBuffer: options.maxBuffer || DEFAULT_MAX_BUFFER,
			...spawnExtra,
			...(timeoutSeconds > 0 ? { timeout: timeoutSeconds * 1000 } : {}),
		});

		const processEvidence = collectArtifacts ? processArtifacts(request, config, spawnResult) : {};
		const context = { request, config, commandSpec, cwd, spawnResult };

		if (spawnResult.error?.code === 'ENOENT') {
			return outcome(request, buildNotFound(context));
		}

		if (spawnResult.error) {
			const timedOut = spawnResult.error.code === 'ETIMEDOUT';
			return outcome(request, buildSpawnError(context, timedOut));
		}

		if (spawnResult.status === 0) {
			return outcome(request, { ...buildSuccess(context), ...processEvidence });
		}

		return outcome(request, { ...buildFailure(context), ...processEvidence });
	}

	return { execute, outcome, validationFailure };
}

function timeoutSecondsFromLimits(limits = {}, fallbackSeconds = 0) {
	if (limits.timeout_ms || limits.max_runtime_ms) {
		return Math.ceil(Number(limits.timeout_ms || limits.max_runtime_ms) / 1000);
	}
	return Number(limits.task_timeout_seconds || limits.taskTimeoutSeconds || fallbackSeconds || 0);
}

function resolveCwd(request = {}, config = {}) {
	return config.cwd || request.workspace_path || request.workspace?.path || process.cwd();
}

function safeFileSegment(value) {
	return String(value || 'agent-task').replace(/[^a-z0-9._-]+/gi, '-').replace(/^-+|-+$/g, '') || 'agent-task';
}

function redactSecrets(content, secretEnvNames) {
	let redacted = content;
	for (const name of secretEnvNames) {
		const value = process.env[name];
		if (value) {
			redacted = redacted.split(value).join('[redacted]');
		}
	}
	return redacted;
}

module.exports = {
	createCliAgentTaskExecutor,
	timeoutSecondsFromLimits,
	resolveCwd,
	safeFileSegment,
	redactSecrets,
};
