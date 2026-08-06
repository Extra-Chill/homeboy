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
const { adapterRuntimeToolRequest } = require('../../lib/runtime-tool-adapter');

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
	'runtime_tool_attachment',
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
		invocation: options.invocation || {
			schema: 'homeboy/command-invocation/v1',
			argv: ['node', '{{runtime_path}}/scripts/agent/homeboy-claude-code-agent-task-executor.cjs'],
			display: 'node {{runtime_path}}/scripts/agent/homeboy-claude-code-agent-task-executor.cjs',
		},
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
		},
		artifact_contract: {
			patch: ['git-diff', 'patch'],
			report: ['json', 'markdown'],
		},
		status: 'available',
		integration_contract: 'homeboy-claude-code-agent-task/v1',
	};
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

const { execute: executeClaudeCodeAgentTask, outcome, validationFailure } = createCliAgentTaskExecutor({
	backend: 'claude-code',
	runtime: 'claude-code',
	providerId: CLAUDE_CODE_PROVIDER_ID,
	providerLabel: 'Claude Code agent',
	defaultSummary: 'Claude Code agent task executor failed before producing a detailed outcome.',
	secretEnv: CLAUDE_CODE_SECRET_ENV,
	artifactProvider: 'claude-code',
	collectArtifacts: true,
	resolveCommandSpec,
	preflight: (request) => {
		if (!process.env.AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN) {
			return {
				status: 'provider_error',
				failure_classification: 'provider',
				failure_code: 'agent_task.claude_code_oauth_missing',
				summary: 'Claude Code OAuth refresh token is missing.',
				diagnostics: [{ classification: 'provider_setup', message: 'Set AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN for the provider adapter.' }],
			};
		}
		return null;
	},
	buildArgs: (request, config, commandSpec) => commandSpec.args,
	buildSpawn: (request, config, options) => ({
		env: cliAgentTaskSpawnEnv(request, options, {
			allowlist: ['HOMEBOY_CLAUDE_CODE_AGENT_TASK_COMMAND', 'HOMEBOY_CLAUDE_CODE_AGENT_TASK_COMMAND_ARGS'],
			secretEnv: CLAUDE_CODE_SECRET_ENV,
		}),
		input: JSON.stringify(adapterRuntimeToolRequest(request)),
	}),
	messages: {
		invalidRequest: { code: 'agent_task.invalid_claude_code_request', summary: 'Claude Code request validation failed.' },
		invalidCommand: { code: 'agent_task.invalid_claude_code_command', summary: 'Claude Code command configuration is invalid.' },
		notFound: { code: 'agent_task.claude_code_command_not_found', summary: 'Claude Code adapter command was not found.', hint: 'Set executor.config.command to an installed adapter command.' },
		timeout: { code: 'agent_task.claude_code_timeout', summary: 'Claude Code execution timed out.' },
		spawnFailed: { code: 'agent_task.claude_code_spawn_failed', summary: 'Claude Code adapter process failed to start or complete.' },
		success: { summary: 'Claude Code adapter completed successfully.', diag: 'Claude Code adapter exited with status 0.' },
		failed: { code: 'agent_task.claude_code_failed', summary: 'Claude Code adapter execution failed.', diag: (status) => `Claude Code adapter exited with status ${status}.` },
	},
});

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
