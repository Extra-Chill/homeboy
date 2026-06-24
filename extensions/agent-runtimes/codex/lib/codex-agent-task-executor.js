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
	AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
	agentTaskProviderContractFields,
	extendRedactedMetadataKeys,
	providerSecretEnvRequirement,
} = require('../../../runtime-agent-ci/lib/agent-task-provider-contract');
const {
	normalizeAgentTaskOutcome,
} = require('../../../runtime-agent-ci/lib/agent-task-outcome-normalizer');

const CODEX_RUNTIME_ID = 'codex';
const CODEX_PROVIDER_ID = 'codex.agent-task-executor';
const CODEX_PROVIDER_LABEL = 'Codex agent task executor';
const CODEX_DEFAULT_COMMAND = 'codex';
const CODEX_DEFAULT_COMMAND_ARGS = ['exec'];
const CODEX_SECRET_ENV = [
	'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
	'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
	'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
	'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
	'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
];

const CODEX_CAPABILITIES = [
	'cli_runtime',
	'repo_workspace',
	'workspace_tools',
	'patch_artifacts',
	'report_artifacts',
	'structured_outcome',
	'provider_owned_auth',
	'provider_owned_session',
	'provider_owned_cancellation',
];

function providerContract(options = {}) {
	const contractFields = agentTaskProviderContractFields();
	contractFields.redacted_metadata_keys = extendRedactedMetadataKeys('codex_auth');

	return {
		schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
		id: options.id || CODEX_PROVIDER_ID,
		label: options.label || CODEX_PROVIDER_LABEL,
		backend: CODEX_RUNTIME_ID,
		runtime: CODEX_RUNTIME_ID,
		command: options.command || 'node {{runtime_path}}/scripts/agent/homeboy-codex-agent-task-executor.cjs',
		...contractFields,
		secret_env_requirements: [providerSecretEnvRequirement('codex', CODEX_SECRET_ENV)],
		capabilities: CODEX_CAPABILITIES,
		workspace_materialization: {
			cwd: 'git_checkout',
		},
		provider_defaults: {
			codex: {
				model: 'gpt-5.5',
				command: CODEX_DEFAULT_COMMAND,
				command_args: [...CODEX_DEFAULT_COMMAND_ARGS],
				secret_env: [...CODEX_SECRET_ENV],
			},
		},
		lifecycle: {
			completion: 'synchronous_process',
			cancellation: 'provider_signal',
			max_concurrency_default: 1,
		},
		artifact_contract: {
			patch: ['git-diff', 'patch'],
			report: ['json', 'markdown'],
		},
		status: 'available',
		integration_contract: 'homeboy-codex-agent-task/v1',
	};
}

function outcome(request = {}, values = {}) {
	return normalizeAgentTaskOutcome(outcomeRequest(request), values, {
		provider: CODEX_PROVIDER_ID,
		providerLabel: 'Codex agent',
		status: values.status || 'provider_error',
		failureClassification: values.failure_classification,
		failureCode: values.failure_code,
		summary: values.summary || 'Codex agent task executor failed before producing a detailed outcome.',
		artifacts: values.artifacts || [],
		evidenceRefs: values.evidence_refs || [],
		metadata: values.metadata || {},
	});
}

function outcomeRequest(request = {}) {
	return request.task_id ? request : { ...request, task_id: 'unknown-task' };
}

function validationFailure(request, message) {
	return outcome(request, {
		status: 'provider_error',
		failure_classification: 'invalid_input',
		failure_code: 'agent_task.invalid_codex_request',
		summary: 'Codex request validation failed.',
		diagnostics: [{ classification: 'request_validation', message }],
	});
}

function executeCodexAgentTask(request = {}, options = {}) {
	const validationError = validateRequest(request);
	if (validationError) {
		return validationFailure(request, validationError);
	}

	const config = request.executor.config || {};
	const commandSpec = resolveCommandSpec(config, options);
	if (commandSpec.error) {
		return outcome(request, {
			status: 'provider_error',
			failure_classification: 'provider',
			failure_code: 'agent_task.invalid_codex_command',
			summary: 'Codex command configuration is invalid.',
			diagnostics: [{ classification: 'provider_setup', message: commandSpec.error }],
		});
	}

	const args = [
		...commandSpec.args,
		...(config.model ? ['--model', config.model] : []),
		request.instructions,
	];
	const timeoutSeconds = timeoutSecondsFromLimits(request.limits, config.timeout_seconds);
	const spawnResult = spawnSync(commandSpec.command, args, {
		cwd: resolveCwd(request, config),
		env: process.env,
		encoding: 'utf8',
		maxBuffer: options.maxBuffer || 10 * 1024 * 1024,
		...(timeoutSeconds > 0 ? { timeout: timeoutSeconds * 1000 } : {}),
	});
	const processEvidence = processArtifacts(request, config, spawnResult, 'codex');

	if (spawnResult.error?.code === 'ENOENT') {
		return outcome(request, {
			status: 'provider_error',
			failure_classification: 'provider',
			failure_code: 'agent_task.codex_command_not_found',
			summary: 'Codex command was not found.',
			diagnostics: [{ classification: 'provider_setup', message: 'Install Codex CLI or configure executor.config.command.' }],
		});
	}

	if (spawnResult.error) {
		const timedOut = spawnResult.error.code === 'ETIMEDOUT';
		return outcome(request, {
			status: timedOut ? 'timeout' : 'provider_error',
			failure_classification: timedOut ? 'timeout' : 'provider',
			failure_code: timedOut ? 'agent_task.codex_timeout' : 'agent_task.codex_spawn_failed',
			summary: timedOut ? 'Codex execution timed out.' : 'Codex process failed to start or complete.',
			diagnostics: [{ classification: timedOut ? 'timeout' : 'provider_setup', message: spawnResult.error.message }],
		});
	}

	if (spawnResult.status === 0) {
		return outcome(request, {
			status: 'succeeded',
			summary: 'Codex completed successfully.',
			diagnostics: [{ classification: 'provider', message: 'Codex CLI exited with status 0.' }],
			metadata: { exit_code: 0 },
			...processEvidence,
		});
	}

	return outcome(request, {
		status: 'failed',
		failure_classification: 'execution_failed',
		failure_code: 'agent_task.codex_failed',
		summary: 'Codex execution failed.',
		diagnostics: [{ classification: 'execution_failed', message: `Codex CLI exited with status ${spawnResult.status}.` }],
		metadata: {
			exit_code: spawnResult.status,
			...(spawnResult.signal ? { signal: spawnResult.signal } : {}),
		},
		...processEvidence,
	});
}

function processArtifacts(request, config, spawnResult, provider) {
	const artifactDir = config.artifacts_path || config.artifactsPath || request.artifacts_path || process.env.HOMEBOY_AGENT_TASK_ARTIFACTS_DIR || process.env.HOMEBOY_RUNTIME_AGENT_ARTIFACTS_DIR || '';
	if (!artifactDir) {
		return {};
	}
	const artifacts = [];
	const evidence_refs = [];
	for (const stream of ['stdout', 'stderr']) {
		const content = redactSecrets(String(spawnResult[stream] || ''), CODEX_SECRET_ENV);
		if (!content) {
			continue;
		}
		const filePath = path.join(artifactDir, `${safeFileSegment(request.task_id)}-${provider}-${stream}.txt`);
		fs.mkdirSync(path.dirname(filePath), { recursive: true });
		fs.writeFileSync(filePath, content);
		const artifact = { id: `${provider}-${stream}`, name: `${provider}-${stream}`, kind: 'provider-process-stream', stream, path: filePath, bytes: Buffer.byteLength(content) };
		artifacts.push(artifact);
		evidence_refs.push({ kind: 'provider-process-stream', label: `${provider} ${stream}`, path: filePath });
	}
	return { artifacts, evidence_refs };
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
	if (request.executor?.backend !== CODEX_RUNTIME_ID) {
		return 'Request executor.backend must be codex.';
	}
	if (request.executor?.runtime !== undefined && request.executor.runtime !== CODEX_RUNTIME_ID) {
		return 'Request executor.runtime must be codex.';
	}
	if (!request.executor.config || typeof request.executor.config !== 'object' || Array.isArray(request.executor.config)) {
		return 'Request executor.config is required.';
	}
	if (!request.instructions || typeof request.instructions !== 'string') {
		return 'Request instructions are required.';
	}
	return null;
}

function timeoutSecondsFromLimits(limits = {}, fallbackSeconds = 0) {
	if (limits.timeout_ms || limits.max_runtime_ms) {
		return Math.ceil(Number(limits.timeout_ms || limits.max_runtime_ms) / 1000);
	}
	return Number(limits.task_timeout_seconds || limits.taskTimeoutSeconds || fallbackSeconds || 0);
}

function resolveCommandSpec(config = {}, options = {}) {
	const configuredCommand = options.command || config.command || process.env.HOMEBOY_CODEX_COMMAND || CODEX_DEFAULT_COMMAND;
	const configuredArgs = options.commandArgs || config.command_args || parseEnvCommandArgs() || CODEX_DEFAULT_COMMAND_ARGS;
	if (typeof configuredCommand !== 'string' || configuredCommand.trim() === '') {
		return { error: 'executor.config.command must be a non-empty string when provided.' };
	}
	if (!Array.isArray(configuredArgs) || configuredArgs.some((arg) => typeof arg !== 'string')) {
		return { error: 'executor.config.command_args must be an array of strings when provided.' };
	}
	return { command: configuredCommand.trim(), args: configuredArgs };
}

function parseEnvCommandArgs() {
	if (!process.env.HOMEBOY_CODEX_COMMAND_ARGS) {
		return null;
	}
	try {
		const value = JSON.parse(process.env.HOMEBOY_CODEX_COMMAND_ARGS);
		return Array.isArray(value) ? value : null;
	} catch {
		return null;
	}
}

function resolveCwd(request = {}, config = {}) {
	return config.cwd || request.workspace_path || request.workspace?.path || process.cwd();
}

module.exports = {
	CODEX_CAPABILITIES,
	CODEX_DEFAULT_COMMAND,
	CODEX_DEFAULT_COMMAND_ARGS,
	CODEX_PROVIDER_ID,
	CODEX_PROVIDER_LABEL,
	CODEX_RUNTIME_ID,
	CODEX_SECRET_ENV,
	executeCodexAgentTask,
	outcome,
	providerContract,
	validationFailure,
};
