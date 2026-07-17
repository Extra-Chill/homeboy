'use strict';

const coreContract = require('./agent-task-provider-contract.generated.json');

const AGENT_TASK_REQUEST_SCHEMA = coreContract.schemas.request;
const AGENT_TASK_OUTCOME_SCHEMA = coreContract.schemas.outcome;
const AGENT_TASK_ARTIFACT_SCHEMA = coreContract.schemas.artifact;
const AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA = coreContract.schemas.artifact_declaration;
const AGENT_TASK_EVIDENCE_REF_SCHEMA = coreContract.schemas.evidence_ref;
const AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA = coreContract.schemas.provider;
const SECRET_ENV_REQUIREMENT_SCHEMA = coreContract.schemas.secret_env_requirement;
const AGENT_TASK_REQUEST_REQUIRED_FIELDS = coreContract.provider_capability.request_required_fields;
const AGENT_TASK_OUTCOME_STATUSES = coreContract.provider_capability.outcome_statuses;
const AGENT_TASK_FAILURE_CLASSIFICATIONS = coreContract.provider_capability.failure_classifications;
const AGENT_TASK_REDACTED_METADATA_KEYS = coreContract.provider_capability.redacted_metadata_keys;

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

const AGENT_TASK_CAPABILITY_BUNDLES = {
  workspace_readwrite: {
    tool_presets: ['runner_workspace'],
    provider_runtime_invocation: {
      operations: {
        workspaceCommand: true,
        workspaceCapture: true,
      },
    },
  },
  github_publication: {
    tool_presets: ['publication'],
    provider_runtime_invocation: {
      operations: {
        workspacePublish: true,
      },
    },
  },
  worktree_pr_iteration: {
    capability_bundles: ['workspace_readwrite', 'github_publication'],
    provider_runtime_invocation: {
      operations: {
        artifactHandoff: true,
        toolCallTranscriptRecord: true,
      },
    },
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

const AGENT_TASK_ARTIFACT_DECLARATION_FIELDS = [
  'name',
  'type',
  'artifact_schema',
  'path',
  'required',
  'description',
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

function expandAgentTaskCapabilityBundles(bundles = [], overrides = {}) {
  const expanded = {};
  const visiting = new Set();
  for (const bundle of normalizeArray(bundles)) {
    mergeCapabilityBundle(expanded, bundle, visiting);
  }
  mergeCapabilityBundleDefinition(expanded, overrides);
  return expanded;
}

function mergeCapabilityBundle(target, bundle, visiting) {
  const bundleName = typeof bundle === 'string' ? bundle : '';
  const definition = bundleName ? AGENT_TASK_CAPABILITY_BUNDLES[bundleName] : bundle;
  if (!definition || typeof definition !== 'object' || Array.isArray(definition)) {
    throw new Error(`Unknown agent task capability bundle: ${bundleName || String(bundle)}`);
  }
  if (bundleName) {
    if (visiting.has(bundleName)) {
      throw new Error(`Circular agent task capability bundle: ${bundleName}`);
    }
    visiting.add(bundleName);
  }
  mergeCapabilityBundleDefinition(target, definition, visiting);
  if (bundleName) {
    visiting.delete(bundleName);
  }
}

function mergeCapabilityBundleDefinition(target, source = {}, visiting = new Set()) {
  for (const nestedBundle of normalizeArray(source.capability_bundles || source.capabilityBundles)) {
    mergeCapabilityBundle(target, nestedBundle, visiting);
  }
  target.tool_presets = uniqueStrings([
    ...normalizeArray(target.tool_presets),
    ...normalizeArray(source.tool_presets || source.toolPresets),
  ]);
  target.provider_runtime_invocation = mergeRuntimeInvocationDescriptor(
    target.provider_runtime_invocation,
    source.provider_runtime_invocation || source.providerRuntimeInvocation || source.runtime_invocation || source.runtimeInvocation
  );
  if (target.tool_presets.length === 0) {
    delete target.tool_presets;
  }
  if (target.provider_runtime_invocation && Object.keys(target.provider_runtime_invocation).length === 0) {
    delete target.provider_runtime_invocation;
  }
  return target;
}

function mergeRuntimeInvocationDescriptor(current = {}, next = {}) {
  if (!next || typeof next !== 'object' || Array.isArray(next)) {
    return current && typeof current === 'object' && !Array.isArray(current) ? current : {};
  }
  const operations = mergeRuntimeInvocationOperations(runtimeInvocationOperations(current), runtimeInvocationOperations(next));
  return {
    ...(current && typeof current === 'object' && !Array.isArray(current) ? current : {}),
    ...next,
    ...(Object.keys(operations).length > 0 ? { operations } : {}),
  };
}

function mergeRuntimeInvocationOperations(current = {}, next = {}) {
  const merged = current && typeof current === 'object' && !Array.isArray(current) ? { ...current } : {};
  for (const [operation, config] of Object.entries(next && typeof next === 'object' && !Array.isArray(next) ? next : {})) {
    if (config && typeof config === 'object' && !Array.isArray(config) && merged[operation] && typeof merged[operation] === 'object' && !Array.isArray(merged[operation])) {
      merged[operation] = { ...merged[operation], ...config };
    } else {
      merged[operation] = config;
    }
  }
  return merged;
}

function runtimeInvocationOperations(value = {}) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return {};
  }
  const operations = value.operations || value.provider_operations || value.providerOperations || value.tasks || value.abilities;
  return operations && typeof operations === 'object' && !Array.isArray(operations) ? operations : {};
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
  AGENT_TASK_ARTIFACT_DECLARATION_FIELDS,
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
  AGENT_TASK_CAPABILITY_BUNDLES,
  AGENT_TASK_TOOL_PRESETS,
  agentTaskArtifactFromRef,
  agentTaskEvidenceRefFromRef,
  agentTaskProviderContractFields,
  expandAgentTaskCapabilityBundles,
  expandAgentTaskToolPresets,
  extendRedactedMetadataKeys,
  providerDefaultsContract,
  providerSecretEnvRequirement,
};
