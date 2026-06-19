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
} = require('../../../runtime-agent-ci/lib/agent-task-provider-contract');

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

const OPENCODE_COMMAND = 'node {{runtime_path}}/scripts/agent/homeboy-opencode-agent-task-executor.cjs';

const OPENCODE_INVOCATION = {
	schema: 'homeboy/command-invocation/v1',
	argv: [
		'node',
		'{{runtime_path}}/scripts/agent/homeboy-opencode-agent-task-executor.cjs',
	],
	display: OPENCODE_COMMAND,
};

const OPENCODE_RUNNER_READINESS = [
	{
		id: 'opencode.executable',
		label: 'OpenCode executable',
		executable: {
			env: ['HOMEBOY_OPENCODE_COMMAND'],
			candidates: ['opencode'],
			version_command: ['--version'],
			install_hint: 'Install OpenCode or set the generic runtime_bin executor config; HOMEBOY_OPENCODE_COMMAND remains a legacy compatibility env alias.',
		},
		remediation: 'Install OpenCode or set the generic runtime_bin executor config; HOMEBOY_OPENCODE_COMMAND remains a legacy compatibility env alias.',
	},
];

const OPENCODE_WORKSPACE_TOOLS = {
	readonly: [
		'workspace_ls',
		'workspace_read',
		'workspace_git_status',
	],
	readwrite: [
		'workspace_run',
		'workspace_write',
		'workspace_edit',
		'workspace_apply_patch',
		'workspace_delete',
		'workspace_git_add',
	],
};

const OPENCODE_WORKSPACE_MATERIALIZATION = {
	cwd: 'git_checkout',
	requires_git: true,
	write_scope: 'workspace',
	artifact_paths: ['.homeboy/opencode'],
};

const OPENCODE_ROLE_ALIASES = {
	artifact_kinds: {
		patch: ['opencode-patch', 'git-diff', 'patch'],
		transcript: ['opencode-transcript', 'agent-runtime-transcript'],
		runtime_log: ['opencode-runtime-log'],
		report: ['opencode-report', 'agent-runtime-report'],
	},
	outputs: {
		provider_run_result: ['opencode_run_result'],
	},
	metadata: {
		provider_run_result: ['opencode_run_result'],
	},
};

const OPENCODE_PROVIDER_DEFAULTS = {
	codex: {
		model: 'gpt-5.5',
		secret_env: [...OPENCODE_SECRET_ENV],
		secret_env_sources: {
			AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: {
				source: 'json-file',
				path: '~/.codex/auth.json',
				field: 'tokens.access_token',
			},
			AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: {
				source: 'json-file',
				path: '~/.codex/auth.json',
				field: 'tokens.refresh_token',
			},
			AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: {
				source: 'json-file-jwt-expiration',
				path: '~/.codex/auth.json',
				field: 'tokens.access_token',
				fallback_fields: ['tokens.expires_at', 'tokens.expiresAt'],
			},
			AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: {
				source: 'json-file',
				path: '~/.codex/auth.json',
				field: 'tokens.account_id',
			},
			AI_PROVIDER_OPENAI_CODEX_FEDRAMP: {
				source: 'json-file',
				path: '~/.codex/auth.json',
				field: 'tokens.fedramp',
				value: 'false',
			},
		},
	},
};

const OPENCODE_PROVIDER_PREFLIGHT = {
	codex: {
		label: 'Codex',
		diagnostic_class: 'opencode.preflight.codex_auth',
		required_secret_env: [
			'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
			'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
			'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
			'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
		],
		optional_secret_env: ['AI_PROVIDER_OPENAI_CODEX_FEDRAMP'],
		refresh_hook: 'codex-oauth-refresh',
		validation_hooks: ['codex-token-expiration'],
		guidance: 'Refresh Codex OAuth credentials before launching OpenCode, for example by signing in with Codex locally so ~/.codex/auth.json contains current tokens, then pass the updated AI_PROVIDER_OPENAI_CODEX_* secret environment values to the OpenCode executor.',
	},
};

function providerContract(options = {}) {
	const contractFields = agentTaskProviderContractFields();
	contractFields.redacted_metadata_keys = extendRedactedMetadataKeys('codex_auth', 'opencode_auth');

	return {
		schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
		id: options.id || OPENCODE_PROVIDER_ID,
		label: options.label || OPENCODE_PROVIDER_LABEL,
		backend: 'opencode',
		runtime_id: 'opencode',
		command: options.command || OPENCODE_COMMAND,
		invocation: options.invocation || OPENCODE_INVOCATION,
		...contractFields,
		secret_env_requirements: [providerSecretEnvRequirement('codex', OPENCODE_SECRET_ENV)],
		capabilities: OPENCODE_CAPABILITIES,
		workspace_materialization: OPENCODE_WORKSPACE_MATERIALIZATION,
		runner_readiness: OPENCODE_RUNNER_READINESS,
		workspace_tools: OPENCODE_WORKSPACE_TOOLS,
		provider_defaults: OPENCODE_PROVIDER_DEFAULTS,
		provider_preflight: OPENCODE_PROVIDER_PREFLIGHT,
		lifecycle: {
			completion: 'synchronous_process',
			cancellation: 'provider_signal',
			max_concurrency_default: 1,
		},
		artifact_contract: {
			patch: ['git-diff', 'patch'],
			report: ['json', 'markdown'],
		},
		role_aliases: OPENCODE_ROLE_ALIASES,
		status: 'active',
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
		artifacts: values.artifacts || [],
		evidence_refs: values.evidence_refs || [],
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
	const timeoutSeconds = timeoutSecondsFromLimits(request.limits, config.timeout_seconds);
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

function timeoutSecondsFromLimits(limits = {}, fallbackSeconds = 0) {
	if (limits.timeout_ms || limits.max_runtime_ms) {
		return Math.ceil(Number(limits.timeout_ms || limits.max_runtime_ms) / 1000);
	}
	return Number(limits.task_timeout_seconds || limits.taskTimeoutSeconds || fallbackSeconds || 0);
}

function resolveCommandSpec(config = {}, options = {}) {
	const configuredCommand = options.command || config.runtime_bin || config.runtimeBin || config.command || process.env.HOMEBOY_OPENCODE_COMMAND || 'opencode';
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
	OPENCODE_COMMAND,
	OPENCODE_INVOCATION,
	OPENCODE_PROVIDER_DEFAULTS,
	OPENCODE_PROVIDER_PREFLIGHT,
	OPENCODE_ROLE_ALIASES,
	OPENCODE_RUNNER_READINESS,
	OPENCODE_WORKSPACE_TOOLS,
	OPENCODE_WORKSPACE_MATERIALIZATION,
	executeOpenCodeAgentTask,
	outcome,
	providerContract,
	validationFailure,
};
