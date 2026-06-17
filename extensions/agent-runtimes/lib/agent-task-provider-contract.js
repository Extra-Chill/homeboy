'use strict';

const AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const AGENT_TASK_OUTCOME_SCHEMA = 'homeboy/agent-task-outcome/v1';
const AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA = 'homeboy/agent-task-executor-provider/v1';
const SECRET_ENV_REQUIREMENT_SCHEMA = 'homeboy/secret-env-requirement/v1';

const AGENT_TASK_REQUEST_REQUIRED_FIELDS = ['schema', 'task_id', 'executor.backend', 'instructions'];

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
];

function agentTaskProviderContractFields() {
  return {
    request_schema: AGENT_TASK_REQUEST_SCHEMA,
    outcome_schema: AGENT_TASK_OUTCOME_SCHEMA,
    request_required_fields: [...AGENT_TASK_REQUEST_REQUIRED_FIELDS],
    outcome_statuses: [...AGENT_TASK_OUTCOME_STATUSES],
    failure_classifications: [...AGENT_TASK_FAILURE_CLASSIFICATIONS],
    redacted_metadata_keys: [...AGENT_TASK_REDACTED_METADATA_KEYS],
  };
}

function providerSecretEnvRequirement(provider, env) {
  return {
    schema: SECRET_ENV_REQUIREMENT_SCHEMA,
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

function providerDefaultsContract(providerDefaults = {}) {
  return Object.fromEntries(Object.entries(providerDefaults).map(([provider, defaults]) => [provider, {
    ...defaults,
    secret_env: normalizeArray(defaults.secret_env),
  }]));
}

function normalizeArray(value) {
  return Array.isArray(value) ? value.filter((entry) => entry !== undefined && entry !== null) : [];
}

module.exports = {
  AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
  AGENT_TASK_FAILURE_CLASSIFICATIONS,
  AGENT_TASK_OUTCOME_SCHEMA,
  AGENT_TASK_OUTCOME_STATUSES,
  AGENT_TASK_REDACTED_METADATA_KEYS,
  AGENT_TASK_REQUEST_SCHEMA,
  agentTaskProviderContractFields,
  providerDefaultsContract,
  providerSecretEnvRequirement,
};
