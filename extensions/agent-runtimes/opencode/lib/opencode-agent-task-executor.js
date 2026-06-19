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

const OPENCODE_PROVIDER_ID = 'opencode.agent-task-executor';
const OPENCODE_PROVIDER_LABEL = 'OpenCode agent task executor';
const OPENCODE_SECRET_ENV = [
	'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
	'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
	'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
	'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
	'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
];

const OPENCODE_CAPABILITIES = [
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
	contractFields.redacted_metadata_keys = extendRedactedMetadataKeys('codex_auth', 'opencode_auth');

	return {
		schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
		id: options.id || OPENCODE_PROVIDER_ID,
		label: options.label || OPENCODE_PROVIDER_LABEL,
		backend: 'opencode',
		command: options.command || 'node {{runtime_path}}/scripts/agent/homeboy-opencode-agent-task-executor.cjs',
		...contractFields,
		secret_env_requirements: [providerSecretEnvRequirement('codex', OPENCODE_SECRET_ENV)],
		capabilities: OPENCODE_CAPABILITIES,
		workspace_materialization: {
			cwd: 'git_checkout',
		},
		provider_defaults: {
			codex: {
				model: 'gpt-5.5',
				secret_env: [...OPENCODE_SECRET_ENV],
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
		integration_contract: 'homeboy-opencode-agent-task/v1',
	};
}

function outcome(request = {}, values = {}) {
	return {
		schema: AGENT_TASK_OUTCOME_SCHEMA,
		task_id: request.task_id || '',
		status: values.status || 'provider_error',
		...(values.failure_classification ? { failure_classification: values.failure_classification } : {}),
		...(values.failure_code ? { failure_code: values.failure_code } : {}),
		summary: values.summary || 'OpenCode agent task executor failed before producing a detailed outcome.',
		diagnostics: values.diagnostics || [],
		artifacts: [],
		evidence_refs: [],
		metadata: {
			provider: OPENCODE_PROVIDER_ID,
			...(values.metadata || {}),
		},
	};
}

function validationFailure(request, message) {
	return outcome(request, {
		status: 'provider_error',
		failure_classification: 'invalid_input',
		failure_code: 'agent_task.invalid_opencode_request',
		summary: 'OpenCode request validation failed.',
		diagnostics: [{ classification: 'request_validation', message }],
	});
}

function executeOpenCodeAgentTask(request = {}, options = {}) {
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
			failure_code: 'agent_task.invalid_opencode_command',
			summary: 'OpenCode command configuration is invalid.',
			diagnostics: [{ classification: 'provider_setup', message: commandSpec.error }],
		});
	}

	const args = [
		...commandSpec.args,
		'run',
		...(config.model ? ['--model', config.model] : []),
		...(config.agent ? ['--agent', config.agent] : []),
		...(config.variant ? ['--variant', config.variant] : []),
		...(config.title ? ['--title', config.title] : []),
		request.instructions,
	];
	const timeoutSeconds = Number(request.limits?.task_timeout_seconds || config.timeout_seconds || 0);
	const spawnResult = spawnSync(commandSpec.command, args, {
		cwd: resolveCwd(request, config),
		env: process.env,
		encoding: 'utf8',
		maxBuffer: options.maxBuffer || 10 * 1024 * 1024,
		...(timeoutSeconds > 0 ? { timeout: timeoutSeconds * 1000 } : {}),
	});

	if (spawnResult.error?.code === 'ENOENT') {
		return outcome(request, {
			status: 'provider_error',
			failure_classification: 'provider',
			failure_code: 'agent_task.opencode_command_not_found',
			summary: 'OpenCode command was not found.',
			diagnostics: [{ classification: 'provider_setup', message: 'Install opencode or configure executor.config.command.' }],
		});
	}

	if (spawnResult.error) {
		const timedOut = spawnResult.error.code === 'ETIMEDOUT';
		return outcome(request, {
			status: timedOut ? 'timeout' : 'provider_error',
			failure_classification: timedOut ? 'timeout' : 'provider',
			failure_code: timedOut ? 'agent_task.opencode_timeout' : 'agent_task.opencode_spawn_failed',
			summary: timedOut ? 'OpenCode execution timed out.' : 'OpenCode process failed to start or complete.',
			diagnostics: [{ classification: timedOut ? 'timeout' : 'provider_setup', message: spawnResult.error.message }],
		});
	}

	if (spawnResult.status === 0) {
		return outcome(request, {
			status: 'succeeded',
			summary: 'OpenCode completed successfully.',
			diagnostics: [{ classification: 'provider', message: 'OpenCode CLI exited with status 0.' }],
			metadata: { exit_code: 0 },
		});
	}

	return outcome(request, {
		status: 'failed',
		failure_classification: 'execution_failed',
		failure_code: 'agent_task.opencode_failed',
		summary: 'OpenCode execution failed.',
		diagnostics: [{ classification: 'execution_failed', message: `OpenCode CLI exited with status ${spawnResult.status}.` }],
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
	if (request.executor?.backend !== 'opencode') {
		return 'Request executor.backend must be opencode.';
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
	const configuredCommand = options.command || config.command || process.env.HOMEBOY_OPENCODE_COMMAND || 'opencode';
	const configuredArgs = options.commandArgs || config.command_args || parseEnvCommandArgs();
	if (typeof configuredCommand !== 'string' || configuredCommand.trim() === '') {
		return { error: 'executor.config.command must be a non-empty string when provided.' };
	}
	if (!Array.isArray(configuredArgs) || configuredArgs.some((arg) => typeof arg !== 'string')) {
		return { error: 'executor.config.command_args must be an array of strings when provided.' };
	}
	return { command: configuredCommand.trim(), args: configuredArgs };
}

function parseEnvCommandArgs() {
	if (!process.env.HOMEBOY_OPENCODE_COMMAND_ARGS) {
		return [];
	}
	try {
		const value = JSON.parse(process.env.HOMEBOY_OPENCODE_COMMAND_ARGS);
		return Array.isArray(value) ? value : [];
	} catch {
		return [];
	}
}

function resolveCwd(request = {}, config = {}) {
	return config.cwd || request.workspace_path || request.workspace?.path || process.cwd();
}

module.exports = {
	OPENCODE_PROVIDER_ID,
	OPENCODE_PROVIDER_LABEL,
	OPENCODE_SECRET_ENV,
	executeOpenCodeAgentTask,
	outcome,
	providerContract,
	validationFailure,
};
