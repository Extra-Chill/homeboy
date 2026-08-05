'use strict';

/**
 * Internal dependencies
 */
const {
	AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
	agentTaskProviderContractFields,
} = require('../../../agent-task-contracts');
const {
	cliAgentTaskSpawnEnv,
	createCliAgentTaskExecutor,
} = require('../../lib/cli-agent-task-executor');

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
		invocation: options.invocation || {
			schema: 'homeboy/command-invocation/v1',
			argv: ['node', '{{runtime_path}}/scripts/agent/homeboy-pi-agent-task-executor.cjs'],
			display: 'node {{runtime_path}}/scripts/agent/homeboy-pi-agent-task-executor.cjs',
		},
		...agentTaskProviderContractFields(),
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
		},
		status: 'experimental',
		integration_contract: 'homeboy-pi-agent-task/v1',
	};
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

function piMetadata(context, extra = {}) {
	return {
		command: context.commandSpec.command,
		args_count: context.commandSpec.args.length,
		cwd: context.cwd,
		...extra,
	};
}

const { execute: executePiAgentTask, outcome, validationFailure } = createCliAgentTaskExecutor({
	backend: PI_BACKEND,
	runtime: PI_BACKEND,
	providerId: PI_PROVIDER_ID,
	providerLabel: 'Pi agent',
	defaultSummary: 'Pi agent task executor did not produce a detailed outcome.',
	requireConfig: false,
	emitArtifacts: true,
	timeoutFallback: (config) => config.timeout_seconds || DEFAULT_TIMEOUT_SECONDS,
	resolveCommandSpec,
	buildArgs: (request, config, commandSpec) => commandSpec.args,
	buildSpawn: (request, config, options) => {
		const requestJson = JSON.stringify(request);
		return {
			env: cliAgentTaskSpawnEnv(request, {
				...options,
				env_overrides: {
					...options.env_overrides,
					...options.envOverrides,
				HOMEBOY_AGENT_TASK_REQUEST: requestJson,
			},
			}, { allowlist: ['HOMEBOY_PI_COMMAND', 'HOMEBOY_PI_COMMAND_ARGS'] }),
			input: requestJson,
		};
	},
	onEmptyCommand: () => ({
		status: 'no_op',
		summary: 'Pi runtime is not configured with an explicit command.',
		diagnostics: [{
			classification: 'provider_setup',
			message: 'Set executor.config.command or HOMEBOY_PI_COMMAND when the Pi runtime contract is available.',
		}],
		metadata: { configured: false },
	}),
	notFoundOutcome: (context) => ({
		status: 'provider_error',
		failure_classification: 'provider',
		failure_code: 'agent_task.pi_command_not_found',
		summary: 'Pi command was not found.',
		diagnostics: [{ classification: 'provider_setup', message: 'Configure executor.config.command with an installed Pi runtime adapter command.' }],
		metadata: piMetadata(context),
	}),
	spawnErrorOutcome: (context, timedOut) => ({
		status: timedOut ? 'timeout' : 'provider_error',
		failure_classification: timedOut ? 'timeout' : 'provider',
		failure_code: timedOut ? 'agent_task.pi_timeout' : 'agent_task.pi_spawn_failed',
		summary: timedOut ? 'Pi command timed out.' : 'Pi command failed to start or complete.',
		diagnostics: [{ classification: timedOut ? 'timeout' : 'provider_setup', message: context.spawnResult.error.message }],
		metadata: piMetadata(context),
	}),
	successOutcome: (context) => ({
		status: 'no_op',
		summary: 'Pi command completed; no Pi-specific outcome contract is implemented yet.',
		diagnostics: [{ classification: 'provider', message: 'Configured Pi command exited with status 0.' }],
		metadata: piMetadata(context, { exit_code: 0 }),
	}),
	failureOutcome: (context) => ({
		status: 'provider_error',
		failure_classification: 'provider',
		failure_code: 'agent_task.pi_command_failed',
		summary: 'Pi command failed before producing a normalized outcome.',
		diagnostics: [{ classification: 'provider', message: `Configured Pi command exited with status ${context.spawnResult.status}.` }],
		metadata: piMetadata(context, {
			exit_code: context.spawnResult.status,
			...(context.spawnResult.signal ? { signal: context.spawnResult.signal } : {}),
		}),
	}),
	messages: {
		invalidRequest: { code: 'agent_task.invalid_pi_request', summary: 'Pi request validation failed.' },
		invalidCommand: { code: 'agent_task.invalid_pi_command', summary: 'Pi command configuration is invalid.' },
	},
});

module.exports = {
	PI_BACKEND,
	PI_PROVIDER_ID,
	PI_PROVIDER_LABEL,
	executePiAgentTask,
	outcome,
	providerContract,
	validationFailure,
};
