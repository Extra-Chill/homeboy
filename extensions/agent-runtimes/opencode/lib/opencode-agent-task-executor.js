'use strict';

const AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const AGENT_TASK_OUTCOME_SCHEMA = 'homeboy/agent-task-outcome/v1';
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
];

const AGENT_TASK_OUTCOME_STATUSES = [
	'succeeded',
	'failed',
	'no_op',
	'unable_to_remediate',
	'timeout',
	'provider_error',
];

const AGENT_TASK_FAILURE_CLASSIFICATIONS = [
	'provider',
	'timeout',
	'execution_failed',
];

const AGENT_TASK_REDACTED_METADATA_KEYS = [
	'secret_env_values',
	'secretEnvValues',
	'secrets',
	'codex_auth',
	'opencode_auth',
];

function providerContract(options = {}) {
	return {
		schema: 'homeboy/agent-task-executor-provider/v1',
		id: options.id || OPENCODE_PROVIDER_ID,
		label: options.label || OPENCODE_PROVIDER_LABEL,
		backend: 'opencode',
		command: options.command || 'node {{runtime_path}}/scripts/agent/homeboy-opencode-agent-task-executor.cjs',
		request_schema: AGENT_TASK_REQUEST_SCHEMA,
		outcome_schema: AGENT_TASK_OUTCOME_SCHEMA,
		request_required_fields: ['schema', 'task_id', 'executor.backend', 'instructions'],
		outcome_statuses: AGENT_TASK_OUTCOME_STATUSES,
		failure_classifications: AGENT_TASK_FAILURE_CLASSIFICATIONS,
		redacted_metadata_keys: AGENT_TASK_REDACTED_METADATA_KEYS,
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
		status: 'experimental',
		integration_contract: 'homeboy-opencode-agent-task/v1',
		runtime_gap_trackers: ['https://github.com/Extra-Chill/homeboy-extensions/issues/967'],
	};
}

function providerSecretEnvRequirement(provider, env) {
	return {
		schema: 'homeboy/secret-env-requirement/v1',
		source: 'provider_default',
		env,
		when: {
			any: [
				{ path: 'executor.config.provider', equals: provider },
				{ path: 'executor.provider', equals: provider },
				{ path: 'provider', equals: provider },
			],
		},
	};
}

function experimentalOutcome(request = {}) {
	return {
		schema: AGENT_TASK_OUTCOME_SCHEMA,
		task_id: request.task_id || '',
		status: 'provider_error',
		failure_classification: 'provider',
		failure_code: 'agent_task.opencode_executor_not_implemented',
		summary: 'OpenCode agent task executor is registered as an experimental provider contract only; process execution is not implemented yet.',
		artifacts: [],
		evidence_refs: [],
		metadata: {
			provider: OPENCODE_PROVIDER_ID,
			issue: 'https://github.com/Extra-Chill/homeboy-extensions/issues/967',
		},
	};
}

module.exports = {
	OPENCODE_PROVIDER_ID,
	OPENCODE_PROVIDER_LABEL,
	OPENCODE_SECRET_ENV,
	experimentalOutcome,
	providerContract,
};
