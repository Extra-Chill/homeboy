'use strict';

const AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const AGENT_TASK_OUTCOME_SCHEMA = 'homeboy/agent-task-outcome/v1';
const AGENT_TASK_ARTIFACT_SCHEMA = 'homeboy/agent-task-artifact/v1';
const AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA = 'homeboy/agent-task-artifact-declaration/v1';
const AGENT_TASK_EVIDENCE_REF_SCHEMA = 'homeboy/agent-task-evidence-ref/v1';
const AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA = 'homeboy/agent-task-executor-provider/v1';
const SECRET_ENV_REQUIREMENT_SCHEMA = 'homeboy/secret-env-requirement/v1';

const AGENT_TASK_REQUEST_REQUIRED_FIELDS = ['schema', 'task_id', 'executor.backend', 'executor.runtime', 'instructions'];

const AGENT_TASK_OUTCOME_STATUSES = [
  'succeeded',
  'no_op',
  'unable_to_remediate',
  'provider_error',
  'timeout',
  'failed',
  'follow_up_issue',
  'cancelled',
];

const AGENT_TASK_FAILURE_CLASSIFICATIONS = [
  'provider',
  'timeout',
  'policy_denied',
  'capability_missing',
  'invalid_input',
  'execution_failed',
  'unknown',
];

const AGENT_TASK_REDACTED_METADATA_KEYS = [
  'secret_env_values',
  'secretEnvValues',
  'secrets',
];

const AGENT_TASK_SECRET_SELECTOR_PATHS = [
  'executor.config.provider',
  'executor.provider',
  'provider',
];

const AGENT_TASK_TOOL_PRESETS = {
  runner_workspace: {
    workspace_tools: {
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
    },
  },
  publication: {
    publication_tools: [
      'publication_prepare',
      'publication_publish',
      'publication_status',
    ],
  },
};

const AGENT_TASK_ARTIFACT_FIELDS = [
  'schema',
  'id',
  'kind',
  'name',
  'path',
  'url',
  'mime',
  'size_bytes',
  'sha256',
  'metadata',
];

const AGENT_TASK_EVIDENCE_REF_FIELDS = [
  'kind',
  'uri',
  'label',
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
      any: AGENT_TASK_SECRET_SELECTOR_PATHS.map((selectorPath) => ({ path: selectorPath, equals: provider })),
    },
  };
}

function extendRedactedMetadataKeys(...keys) {
  return Array.from(new Set([...AGENT_TASK_REDACTED_METADATA_KEYS, ...keys.flatMap(normalizeList)]));
}

function agentTaskArtifactFromRef(ref = {}, index = 0, sanitizeMetadata = passthroughObject) {
  const id = ref.id || ref.sha256 || ref.path || ref.url || `agent-task-artifact-${index + 1}`;
  return {
    schema: AGENT_TASK_ARTIFACT_SCHEMA,
    id,
    kind: ref.kind || ref.type || 'agent_task_artifact',
    name: ref.name,
    path: ref.path || ref.directory,
    url: ref.url,
    mime: ref.mime,
    size_bytes: ref.size_bytes,
    sha256: ref.sha256,
    metadata: sanitizeMetadata(ref.metadata || {}),
  };
}

function agentTaskEvidenceRefFromRef(ref = {}, fallbackKind = 'agent_task_evidence') {
  return {
    kind: ref.kind || ref.type || fallbackKind,
    uri: ref.uri || ref.url || ref.path,
    label: ref.label || ref.name,
  };
}

function providerDefaultsContract(providerDefaults = {}) {
  return Object.fromEntries(Object.entries(providerDefaults).map(([provider, defaults]) => [provider, {
    ...defaults,
    secret_env: normalizeArray(defaults.secret_env),
  }]));
}

function expandAgentTaskToolPresets(presets = [], overrides = {}) {
  const expanded = {};
  for (const preset of normalizeArray(presets)) {
    const definition = AGENT_TASK_TOOL_PRESETS[preset];
    if (!definition) {
      throw new Error(`Unknown agent task tool preset: ${preset}`);
    }
    mergeToolPreset(expanded, definition);
  }
  mergeToolPreset(expanded, overrides);
  return expanded;
}

function mergeToolPreset(target, source = {}) {
  if (source.workspace_tools) {
    target.workspace_tools = mergeWorkspaceTools(target.workspace_tools, source.workspace_tools);
  }
  if (source.publication_tools) {
    target.publication_tools = uniqueStrings([
      ...normalizeArray(target.publication_tools),
      ...normalizeArray(source.publication_tools),
    ]);
  }
  return target;
}

function mergeWorkspaceTools(current = {}, next = {}) {
  return {
    readonly: uniqueStrings([
      ...normalizeArray(current.readonly),
      ...normalizeArray(next.readonly),
    ]),
    readwrite: uniqueStrings([
      ...normalizeArray(current.readwrite),
      ...normalizeArray(next.readwrite),
    ]),
  };
}

function uniqueStrings(values) {
  return Array.from(new Set(values.filter((value) => typeof value === 'string' && value.trim() !== '')));
}

function normalizeArray(value) {
  return Array.isArray(value) ? value.filter((entry) => entry !== undefined && entry !== null) : [];
}

function normalizeList(value) {
  return Array.isArray(value) ? normalizeArray(value) : normalizeArray([value]);
}

function passthroughObject(value) {
  return value;
}

module.exports = {
  AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA,
  AGENT_TASK_ARTIFACT_FIELDS,
  AGENT_TASK_ARTIFACT_SCHEMA,
  AGENT_TASK_EVIDENCE_REF_FIELDS,
  AGENT_TASK_EVIDENCE_REF_SCHEMA,
  AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
  AGENT_TASK_FAILURE_CLASSIFICATIONS,
  AGENT_TASK_OUTCOME_SCHEMA,
  AGENT_TASK_OUTCOME_STATUSES,
  AGENT_TASK_REDACTED_METADATA_KEYS,
  AGENT_TASK_REQUEST_SCHEMA,
  AGENT_TASK_SECRET_SELECTOR_PATHS,
  AGENT_TASK_TOOL_PRESETS,
  agentTaskArtifactFromRef,
  agentTaskEvidenceRefFromRef,
  agentTaskProviderContractFields,
  expandAgentTaskToolPresets,
  extendRedactedMetadataKeys,
  providerDefaultsContract,
  providerSecretEnvRequirement,
};
