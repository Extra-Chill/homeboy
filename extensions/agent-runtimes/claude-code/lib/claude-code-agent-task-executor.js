'use strict';

/**
 * External dependencies
 */
const { spawnSync } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
	AGENT_TASK_REQUEST_SCHEMA,
	AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
	AGENT_TASK_OUTCOME_SCHEMA,
	agentTaskProviderContractFields,
	extendRedactedMetadataKeys,
	providerSecretEnvRequirement,
} = require('../../lib/agent-task-provider-contract');

const CLAUDE_CODE_PROVIDER_ID = 'claude-code.agent-task-executor';
const CLAUDE_CODE_PROVIDER_LABEL = 'Claude Code agent task executor';
const CLAUDE_CODE_REQUIRED_SECRET_ENV = [
	'AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN',
];
const CLAUDE_CODE_OPTIONAL_SECRET_ENV = [
	'AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN',
	'AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT',
];
const CLAUDE_CODE_SECRET_ENV = [
	...CLAUDE_CODE_REQUIRED_SECRET_ENV,
	...CLAUDE_CODE_OPTIONAL_SECRET_ENV,
];

const CLAUDE_CODE_CAPABILITIES = [
	'cli_runtime',
	'repo_workspace',
	'workspace_tools',
	'patch_artifacts',
	'report_artifacts',
	'structured_outcome',
	'provider_owned_auth',
	'provider_owned_session',
	'provider_owned_cancellation',
	'nested_orchestrator',
];

function providerContract(options = {}) {
	const contractFields = agentTaskProviderContractFields();
	contractFields.redacted_metadata_keys = extendRedactedMetadataKeys('claude_code_auth');

	return {
		schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
		id: options.id || CLAUDE_CODE_PROVIDER_ID,
		label: options.label || CLAUDE_CODE_PROVIDER_LABEL,
		backend: 'claude-code',
		runtime: 'claude-code',
		command: options.command || 'node {{runtime_path}}/scripts/agent/homeboy-claude-code-agent-task-executor.cjs',
		...contractFields,
		secret_env_requirements: [providerSecretEnvRequirement('claude-code', CLAUDE_CODE_REQUIRED_SECRET_ENV)],
		capabilities: CLAUDE_CODE_CAPABILITIES,
		workspace_materialization: {
			cwd: 'git_checkout',
		},
		provider_defaults: {
			'claude-code': {
				secret_env: [...CLAUDE_CODE_SECRET_ENV],
				required_secret_env: [...CLAUDE_CODE_REQUIRED_SECRET_ENV],
				optional_secret_env: [...CLAUDE_CODE_OPTIONAL_SECRET_ENV],
			},
		},
		provider_preflight: {
			'claude-code': {
				label: 'Claude Code',
				diagnostic_class: 'claude_code.preflight.oauth',
				required_secret_env: [...CLAUDE_CODE_REQUIRED_SECRET_ENV],
				optional_secret_env: [...CLAUDE_CODE_OPTIONAL_SECRET_ENV],
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
		integration_contract: 'homeboy-claude-code-agent-task/v1',
	};
}

function outcome(request = {}, values = {}) {
	return {
		schema: AGENT_TASK_OUTCOME_SCHEMA,
		task_id: request.task_id || '',
		status: values.status || 'provider_error',
		...(values.failure_classification ? { failure_classification: values.failure_classification } : {}),
		...(values.failure_code ? { failure_code: values.failure_code } : {}),
		summary: values.summary || 'Claude Code agent task executor failed before producing a detailed outcome.',
		diagnostics: values.diagnostics || [],
		artifacts: [],
		evidence_refs: [],
		metadata: {
			provider: CLAUDE_CODE_PROVIDER_ID,
			...(values.metadata || {}),
		},
	};
}

function validationFailure(request, message) {
	return outcome(request, {
		status: 'provider_error',
		failure_classification: 'invalid_input',
		failure_code: 'agent_task.invalid_claude_code_request',
		summary: 'Claude Code request validation failed.',
		diagnostics: [{ classification: 'request_validation', message }],
	});
}

function executeClaudeCodeAgentTask(request = {}, options = {}) {
	const validationError = validateRequest(request);
	if (validationError) {
		return validationFailure(request, validationError);
	}

	if (!process.env.AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN) {
		return outcome(request, {
			status: 'provider_error',
			failure_classification: 'provider',
			failure_code: 'agent_task.claude_code_oauth_missing',
			summary: 'Claude Code OAuth refresh token is missing.',
			diagnostics: [{ classification: 'provider_setup', message: 'Set AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN for the provider adapter.' }],
		});
	}

	const config = request.executor.config || {};
	const commandSpec = resolveCommandSpec(config, options);
	if (commandSpec.error) {
		return outcome(request, {
			status: 'provider_error',
			failure_classification: commandSpec.classification || 'provider',
			failure_code: commandSpec.code || 'agent_task.invalid_claude_code_command',
			summary: commandSpec.summary || 'Claude Code command configuration is invalid.',
			diagnostics: [{ classification: 'provider_setup', message: commandSpec.error }],
		});
	}

	const timeoutSeconds = Number(request.limits?.task_timeout_seconds || config.timeout_seconds || 0);
	const spawnResult = spawnSync(commandSpec.command, commandSpec.args, {
		cwd: resolveCwd(request, config),
		env: process.env,
		input: JSON.stringify(request),
		encoding: 'utf8',
		maxBuffer: options.maxBuffer || 10 * 1024 * 1024,
		...(timeoutSeconds > 0 ? { timeout: timeoutSeconds * 1000 } : {}),
	});

	if (spawnResult.error?.code === 'ENOENT') {
		return outcome(request, {
			status: 'provider_error',
			failure_classification: 'provider',
			failure_code: 'agent_task.claude_code_command_not_found',
			summary: 'Claude Code adapter command was not found.',
			diagnostics: [{ classification: 'provider_setup', message: 'Set executor.config.command to an installed adapter command.' }],
		});
	}

	if (spawnResult.error) {
		const timedOut = spawnResult.error.code === 'ETIMEDOUT';
		return outcome(request, {
			status: timedOut ? 'timeout' : 'provider_error',
			failure_classification: timedOut ? 'timeout' : 'provider',
			failure_code: timedOut ? 'agent_task.claude_code_timeout' : 'agent_task.claude_code_spawn_failed',
			summary: timedOut ? 'Claude Code execution timed out.' : 'Claude Code adapter process failed to start or complete.',
			diagnostics: [{ classification: timedOut ? 'timeout' : 'provider_setup', message: spawnResult.error.message }],
		});
	}

	if (spawnResult.status === 0) {
		return outcome(request, {
			status: 'succeeded',
			summary: 'Claude Code adapter completed successfully.',
			diagnostics: [{ classification: 'provider', message: 'Claude Code adapter exited with status 0.' }],
			metadata: { exit_code: 0 },
		});
	}

	return outcome(request, {
		status: 'failed',
		failure_classification: 'execution_failed',
		failure_code: 'agent_task.claude_code_failed',
		summary: 'Claude Code adapter execution failed.',
		diagnostics: [{ classification: 'execution_failed', message: `Claude Code adapter exited with status ${spawnResult.status}.` }],
		metadata: {
			exit_code: spawnResult.status,
			...(spawnResult.signal ? { signal: spawnResult.signal } : {}),
		},
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
	if (request.executor?.backend !== 'claude-code') {
		return 'Request executor.backend must be claude-code.';
	}
	if (request.executor?.runtime !== 'claude-code') {
		return 'Request executor.runtime must be claude-code.';
	}
	if (!request.executor.config || typeof request.executor.config !== 'object' || Array.isArray(request.executor.config)) {
		return 'Request executor.config is required.';
	}
	if (!request.instructions || typeof request.instructions !== 'string') {
		return 'Request instructions are required.';
	}
	return null;
}

function resolveCommandSpec(config = {}, options = {}) {
	const configuredCommand = options.command || config.command || process.env.HOMEBOY_CLAUDE_CODE_AGENT_TASK_COMMAND || '';
	const configuredArgs = options.commandArgs || config.command_args || parseEnvCommandArgs();
	if (typeof configuredCommand !== 'string' || configuredCommand.trim() === '') {
		return {
			classification: 'capability_missing',
			code: 'agent_task.claude_code_adapter_missing',
			summary: 'Claude Code adapter command is not configured.',
			error: 'Set executor.config.command or HOMEBOY_CLAUDE_CODE_AGENT_TASK_COMMAND to an adapter that accepts AgentTaskRequest JSON on stdin.',
		};
	}
	if (!Array.isArray(configuredArgs) || configuredArgs.some((arg) => typeof arg !== 'string')) {
		return { error: 'executor.config.command_args must be an array of strings when provided.' };
	}
	return { command: configuredCommand.trim(), args: configuredArgs };
}

function parseEnvCommandArgs() {
	if (!process.env.HOMEBOY_CLAUDE_CODE_AGENT_TASK_COMMAND_ARGS) {
		return [];
	}
	try {
		const value = JSON.parse(process.env.HOMEBOY_CLAUDE_CODE_AGENT_TASK_COMMAND_ARGS);
		return Array.isArray(value) ? value : [];
	} catch {
		return [];
	}
}

function resolveCwd(request = {}, config = {}) {
	return config.cwd || request.workspace_path || request.workspace?.path || process.cwd();
}

module.exports = {
	CLAUDE_CODE_OPTIONAL_SECRET_ENV,
	CLAUDE_CODE_PROVIDER_ID,
	CLAUDE_CODE_PROVIDER_LABEL,
	CLAUDE_CODE_REQUIRED_SECRET_ENV,
	CLAUDE_CODE_SECRET_ENV,
	executeClaudeCodeAgentTask,
	outcome,
	providerContract,
	validationFailure,
};
