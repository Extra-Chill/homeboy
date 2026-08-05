'use strict';

/**
 * Internal dependencies
 */
const {
	AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
	agentTaskProviderContractFields,
	extendRedactedMetadataKeys,
	providerSecretEnvRequirement,
} = require('../../../agent-task-contracts');
const {
	cliAgentTaskSpawnEnv,
	createCliAgentTaskExecutor,
} = require('../../lib/cli-agent-task-executor');
const { codexRuntimeToolConfigArgs } = require('../../lib/runtime-tool-adapter');

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
		invocation: options.invocation || {
			schema: 'homeboy/command-invocation/v1',
			argv: ['node', '{{runtime_path}}/scripts/agent/homeboy-codex-agent-task-executor.cjs'],
			display: 'node {{runtime_path}}/scripts/agent/homeboy-codex-agent-task-executor.cjs',
		},
		...contractFields,
		secret_env_requirements: [providerSecretEnvRequirement('codex', CODEX_SECRET_ENV)],
		capabilities: CODEX_CAPABILITIES,
		workspace_materialization: {
			cwd: 'git_checkout',
		},
		provider_defaults: {
			codex: {
				command: CODEX_DEFAULT_COMMAND,
				command_args: [...CODEX_DEFAULT_COMMAND_ARGS],
				secret_env: [...CODEX_SECRET_ENV],
			},
		},
		lifecycle: {
			completion: 'synchronous_process',
			cancellation: 'provider_signal',
		},
		artifact_contract: {
			patch: ['git-diff', 'patch'],
			report: ['json', 'markdown'],
		},
		status: 'available',
		integration_contract: 'homeboy-codex-agent-task/v1',
	};
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

const { execute: executeCodexAgentTask, outcome, validationFailure } = createCliAgentTaskExecutor({
	backend: CODEX_RUNTIME_ID,
	runtime: CODEX_RUNTIME_ID,
	providerId: CODEX_PROVIDER_ID,
	providerLabel: 'Codex agent',
	defaultSummary: 'Codex agent task executor failed before producing a detailed outcome.',
	secretEnv: CODEX_SECRET_ENV,
	artifactProvider: 'codex',
	collectArtifacts: true,
	resolveCommandSpec,
	buildArgs: (request, config, commandSpec, options) => {
		const model = config.model || request.executor?.model || request.model;
		return [
			...commandSpec.args,
			...codexRuntimeToolConfigArgs(request, options.env || process.env),
			...(model ? ['--model', model] : []),
			request.instructions,
		];
	},
	buildSpawn: (request, config, options) => ({
		env: cliAgentTaskSpawnEnv(request, options, {
			allowlist: ['HOMEBOY_CODEX_COMMAND', 'HOMEBOY_CODEX_COMMAND_ARGS'],
			secretEnv: CODEX_SECRET_ENV,
		}),
	}),
	messages: {
		invalidRequest: { code: 'agent_task.invalid_codex_request', summary: 'Codex request validation failed.' },
		invalidCommand: { code: 'agent_task.invalid_codex_command', summary: 'Codex command configuration is invalid.' },
		notFound: { code: 'agent_task.codex_command_not_found', summary: 'Codex command was not found.', hint: 'Install Codex CLI or configure executor.config.command.' },
		timeout: { code: 'agent_task.codex_timeout', summary: 'Codex execution timed out.' },
		spawnFailed: { code: 'agent_task.codex_spawn_failed', summary: 'Codex process failed to start or complete.' },
		success: { summary: 'Codex completed successfully.', diag: 'Codex CLI exited with status 0.' },
		failed: { code: 'agent_task.codex_failed', summary: 'Codex execution failed.', diag: (status) => `Codex CLI exited with status ${status}.` },
	},
});

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
