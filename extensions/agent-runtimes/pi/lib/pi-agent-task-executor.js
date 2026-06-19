'use strict';

/**
 * External dependencies
 */
const { spawnSync } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
	AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
	AGENT_TASK_OUTCOME_SCHEMA,
	AGENT_TASK_REQUEST_SCHEMA,
	agentTaskProviderContractFields,
} = require('../../lib/agent-task-provider-contract');

const PI_PROVIDER_ID = 'pi.agent-task-executor';
const PI_PROVIDER_LABEL = 'Pi agent task executor';
const PI_BACKEND = 'pi';
const DEFAULT_TIMEOUT_SECONDS = 300;

function providerContract(options = {}) {
	return {
		schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
		id: options.id || PI_PROVIDER_ID,
		label: options.label || PI_PROVIDER_LABEL,
		backend: PI_BACKEND,
		runtime: PI_BACKEND,
		command: options.command || 'node {{runtime_path}}/scripts/agent/homeboy-pi-agent-task-executor.cjs',
		...agentTaskProviderContractFields(),
		request_required_fields: ['schema', 'task_id', 'executor.backend', 'executor.runtime', 'instructions'],
		secret_env_requirements: [],
		capabilities: [
			'cli_runtime',
			'workspace_materialization',
			'structured_outcome',
		],
		workspace_materialization: {
			cwd: 'request_workspace',
			requires_git: false,
			write_scope: 'workspace',
		},
		provider_defaults: {},
		lifecycle: {
			completion: 'synchronous_process',
			cancellation: 'process_signal',
			max_concurrency_default: 1,
		},
		status: 'experimental',
		integration_contract: 'homeboy-pi-agent-task/v1',
	};
}

function outcome(request = {}, values = {}) {
	return {
		schema: AGENT_TASK_OUTCOME_SCHEMA,
		task_id: request.task_id || '',
		status: values.status || 'provider_error',
		...(values.failure_classification ? { failure_classification: values.failure_classification } : {}),
		...(values.failure_code ? { failure_code: values.failure_code } : {}),
		summary: values.summary || 'Pi agent task executor did not produce a detailed outcome.',
		diagnostics: values.diagnostics || [],
		artifacts: [],
		evidence_refs: [],
		metadata: {
			provider: PI_PROVIDER_ID,
			...(values.metadata || {}),
		},
	};
}

function validationFailure(request, message) {
	return outcome(request, {
		status: 'provider_error',
		failure_classification: 'invalid_input',
		failure_code: 'agent_task.invalid_pi_request',
		summary: 'Pi request validation failed.',
		diagnostics: [{ classification: 'request_validation', message }],
	});
}

function executePiAgentTask(request = {}, options = {}) {
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
			failure_code: 'agent_task.invalid_pi_command',
			summary: 'Pi command configuration is invalid.',
			diagnostics: [{ classification: 'provider_setup', message: commandSpec.error }],
		});
	}

	if (!commandSpec.command) {
		return outcome(request, {
			status: 'no_op',
			summary: 'Pi runtime is not configured with an explicit command.',
			diagnostics: [{
				classification: 'provider_setup',
				message: 'Set executor.config.command or HOMEBOY_PI_COMMAND when the Pi runtime contract is available.',
			}],
			metadata: { configured: false },
		});
	}

	const timeoutSeconds = Number(request.limits?.task_timeout_seconds || config.timeout_seconds || DEFAULT_TIMEOUT_SECONDS);
	const cwd = resolveCwd(request, config);
	const requestJson = JSON.stringify(request);
	const spawnResult = spawnSync(commandSpec.command, commandSpec.args, {
		cwd,
		env: {
			...process.env,
			HOMEBOY_AGENT_TASK_REQUEST: requestJson,
		},
		input: requestJson,
		encoding: 'utf8',
		maxBuffer: options.maxBuffer || 10 * 1024 * 1024,
		...(timeoutSeconds > 0 ? { timeout: timeoutSeconds * 1000 } : {}),
	});

	if (spawnResult.error?.code === 'ENOENT') {
		return outcome(request, {
			status: 'provider_error',
			failure_classification: 'provider',
			failure_code: 'agent_task.pi_command_not_found',
			summary: 'Pi command was not found.',
			diagnostics: [{ classification: 'provider_setup', message: 'Configure executor.config.command with an installed Pi runtime adapter command.' }],
			metadata: { command: commandSpec.command, args_count: commandSpec.args.length, cwd },
		});
	}

	if (spawnResult.error) {
		const timedOut = spawnResult.error.code === 'ETIMEDOUT';
		return outcome(request, {
			status: timedOut ? 'timeout' : 'provider_error',
			failure_classification: timedOut ? 'timeout' : 'provider',
			failure_code: timedOut ? 'agent_task.pi_timeout' : 'agent_task.pi_spawn_failed',
			summary: timedOut ? 'Pi command timed out.' : 'Pi command failed to start or complete.',
			diagnostics: [{ classification: timedOut ? 'timeout' : 'provider_setup', message: spawnResult.error.message }],
			metadata: { command: commandSpec.command, args_count: commandSpec.args.length, cwd },
		});
	}

	if (spawnResult.status === 0) {
		return outcome(request, {
			status: 'no_op',
			summary: 'Pi command completed; no Pi-specific outcome contract is implemented yet.',
			diagnostics: [{ classification: 'provider', message: 'Configured Pi command exited with status 0.' }],
			metadata: { command: commandSpec.command, args_count: commandSpec.args.length, cwd, exit_code: 0 },
		});
	}

	return outcome(request, {
		status: 'provider_error',
		failure_classification: 'provider',
		failure_code: 'agent_task.pi_command_failed',
		summary: 'Pi command failed before producing a normalized outcome.',
		diagnostics: [{ classification: 'provider', message: `Configured Pi command exited with status ${spawnResult.status}.` }],
		metadata: {
			command: commandSpec.command,
			args_count: commandSpec.args.length,
			cwd,
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
	if (request.executor?.backend !== PI_BACKEND) {
		return `Request executor.backend must be ${PI_BACKEND}.`;
	}
	if (request.executor?.runtime !== PI_BACKEND) {
		return `Request executor.runtime must be ${PI_BACKEND}.`;
	}
	if (!request.instructions || typeof request.instructions !== 'string') {
		return 'Request instructions are required.';
	}
	return null;
}

function resolveCommandSpec(config = {}, options = {}) {
	const configuredCommand = options.command || config.command || process.env.HOMEBOY_PI_COMMAND || '';
	const configuredArgs = options.commandArgs || config.command_args || parseEnvCommandArgs();
	if (typeof configuredCommand !== 'string') {
		return { error: 'executor.config.command must be a string when provided.' };
	}
	if (!Array.isArray(configuredArgs) || configuredArgs.some((arg) => typeof arg !== 'string')) {
		return { error: 'executor.config.command_args must be an array of strings when provided.' };
	}
	return { command: configuredCommand.trim(), args: configuredArgs };
}

function parseEnvCommandArgs() {
	if (!process.env.HOMEBOY_PI_COMMAND_ARGS) {
		return [];
	}
	try {
		const value = JSON.parse(process.env.HOMEBOY_PI_COMMAND_ARGS);
		return Array.isArray(value) ? value : [];
	} catch {
		return [];
	}
}

function resolveCwd(request = {}, config = {}) {
	return config.cwd || request.workspace_path || request.workspace?.path || process.cwd();
}

module.exports = {
	PI_BACKEND,
	PI_PROVIDER_ID,
	PI_PROVIDER_LABEL,
	executePiAgentTask,
	outcome,
	providerContract,
	validationFailure,
};
