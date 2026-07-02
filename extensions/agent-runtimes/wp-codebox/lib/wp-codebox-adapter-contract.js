'use strict';

const {
  providerRuntimeInvocationContract,
  runtimeContractSchemas,
} = require('./wp-codebox-runtime-contract-source');

const RUNTIME_CONTRACT_SCHEMAS = runtimeContractSchemas();
const RUNTIME_INVOCATION_CONTRACT = providerRuntimeInvocationContract();

const WP_CODEBOX_TASK_REQUEST_SCHEMA = 'wp-codebox/task-input/v1';
const WP_CODEBOX_AGENT_FANOUT_REQUEST_SCHEMA = 'wp-codebox/agent-fanout-request/v1';
const WP_CODEBOX_PROVIDER_ID = 'wordpress.codebox-agent-task-executor';
const WP_CODEBOX_PROVIDER_LABEL = 'WP Codebox agent task executor';
const WP_CODEBOX_BACKEND = 'wp-codebox';
const WP_CODEBOX_PROVIDER_RUNTIME_INVOCATION_CONTRACT_SCHEMA = RUNTIME_CONTRACT_SCHEMAS.providerRuntime.invocation;
const WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA = 'wp-codebox/provider-credential-boundary/v1';
const WP_CODEBOX_RECIPE_RUN_CLI_COMMAND = 'recipe-run';
const WP_CODEBOX_WORKSPACE_MOUNT_KIND = 'homeboy-runtime-workspace';

const WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES = {
  ...RUNTIME_INVOCATION_CONTRACT.tasks,
};

const WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES = {
  ...RUNTIME_INVOCATION_CONTRACT.abilities,
};

const WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS = {
  ...RUNTIME_INVOCATION_CONTRACT.result_schemas,
  artifact_result_envelope: RUNTIME_CONTRACT_SCHEMAS.artifact.resultEnvelope,
};

const WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMA_KEYS = {
  workspaceCapture: 'workspace_capture',
  workspaceCommand: 'workspace_command',
  workspacePublish: 'workspace_publication',
  toolCallTranscriptRecord: 'tool_call_transcript',
  artifactHandoff: 'evidence_artifact_envelope',
};

const WP_CODEBOX_PROVIDER_RUNTIME_OPERATION_ALIASES = Object.fromEntries(Object.entries({
  workspaceCapture: [
    'workspaceCapture',
    'workspace_capture',
    WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES.workspaceCapture,
    WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES.workspaceCapture,
  ],
  workspaceCommand: [
    'workspaceCommand',
    'workspace_command',
    WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES.workspaceCommand,
    WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES.workspaceCommand,
  ],
  workspacePublish: [
    'workspacePublish',
    'workspace_publish',
    'workspacePublication',
    'workspace_publication',
    WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES.workspacePublish,
    WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES.workspacePublish,
  ],
  toolCallTranscriptRecord: [
    'toolCallTranscriptRecord',
    'tool_call_transcript_record',
    'toolCallTranscript',
    'tool_call_transcript',
    WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES.toolCallTranscriptRecord,
    WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES.toolCallTranscriptRecord,
  ],
  artifactHandoff: [
    'artifactHandoff',
    'artifact_handoff',
    WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES.artifactHandoff,
    WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES.artifactHandoff,
  ],
}).flatMap(([key, aliases]) => aliases.map((alias) => [alias, key])));

const WP_CODEBOX_ROLE_ALIASES = {
  artifact_roles: {
    artifact_bundle: ['codebox-artifact-bundle', 'artifact-bundle', 'codebox-artifact-directory', 'codebox-session-artifacts'],
    changed_files: ['codebox-changed-files'],
    patch: ['codebox-patch'],
    transcript: ['codebox-transcript', 'agent-runtime-transcript', 'agent-runtime-transcript-summary'],
    runtime_log: ['codebox-runtime-log', 'codebox-recipe-startup-log'],
    command_log: ['codebox-command-log'],
    typed_artifact: ['typed-bundle-output'],
    replay_bundle: ['agent-runtime-replay-bundle'],
    pull_request: ['agent-runtime-pull-request'],
    probe_result: ['codebox-recipe-probe-json', 'recipe-probe-result'],
    screenshot: ['codebox-recipe-screenshot'],
    side_effects: ['codebox-recipe-fake-side-effects'],
    preflight_evidence: ['codebox-command-evidence', 'codebox-agent-task-input'],
  },
  artifact_kinds: {
    patch: ['codebox-patch'],
  },
  artifact_filenames: {
    preflight_evidence: ['homeboy-codebox-task-runner.json'],
  },
  outputs: {
    provider_run_result: ['codebox_run_result'],
  },
  metadata: {
    provider_run_result: ['codebox_run_result'],
  },
};

const WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS = [
  {
    id: 'agent-fanout-request',
    schema: WP_CODEBOX_AGENT_FANOUT_REQUEST_SCHEMA,
    owner: 'wp-codebox',
    adapter_behavior: 'project_homeboy_agent_task_metadata_without_product_assumptions',
    requirement: 'Accept lower-level sandbox-native fanout requests with typed worker, artifact, event, and aggregation contracts. Homeboy owns durable fanout plan/run state; Homeboy Extensions only adapts generic task metadata, workspace, provider/model, secret-env names, progress/evidence callbacks, and artifact declarations into this request shape.',
  },
  {
    id: 'run-agent-task',
    schema: 'wp-codebox/run-agent-task/v1',
    owner: 'wp-codebox',
    adapter_behavior: 'stable_run_agent_task',
    requirement: 'Accept a Codebox-owned run-agent-task request that wraps the prepared task input and returns the stable agent_task_run_result envelope.',
  },
  {
    id: 'provider-credential-boundary',
    schema: WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA,
    owner: 'wp-codebox',
    adapter_behavior: 'forward_secret_env_names_only',
    requirement: 'Resolve provider credentials through provider plugins or parent control-plane filters. Homeboy passes only secret_env names and must not serialize raw provider credentials into request JSON, artifacts, diagnostics, or review output.',
  },
  {
    id: 'runtime-profile',
    schema: RUNTIME_CONTRACT_SCHEMAS.runtimeBoundary.profile,
    owner: 'wp-codebox',
    adapter_behavior: 'forward_profile_payload',
    requirement: 'Consume generic runtime dependencies, provider plugins, overlays, env, and mounts through the public runtime profile payload Homeboy forwards.',
  },
  {
    id: 'parent-tool-bridge',
    schema: 'wp-codebox/parent-tool-bridge/v1',
    owner: 'wp-codebox',
    adapter_behavior: 'declare_requirement_when_missing',
    requirement: 'Expose parent-owned tools inside the sandbox through a Codebox-owned bridge component declared by the public parent-tool-bridge contract.',
  },
  {
    id: 'provider-runtime-invocation',
    schema: WP_CODEBOX_PROVIDER_RUNTIME_INVOCATION_CONTRACT_SCHEMA,
    owner: 'wp-codebox',
    adapter_behavior: 'declare_task_and_ability_names',
    requirement: 'Provide runner workspace, transcript recording, and artifact handoff operations behind stable task/ability names.',
  },
  {
    id: 'artifact-result-envelope',
    schema: WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS.artifact_result_envelope,
    owner: 'wp-codebox',
    adapter_behavior: 'consume_canonical_public_envelope_only',
    requirement: 'Return typed artifacts, evidence refs, and run summaries in stable envelopes so adapters do not parse backend-local artifact layouts. Results that expose private runtime fields without the public envelope fail at the boundary.',
  },
  {
    id: 'artifact-apply-execution',
    schema: 'wp-codebox/artifact-apply-result/v1',
    owner: 'wp-codebox',
    adapter_behavior: 'local_git_apply_until_primitive_exists',
    requirement: 'Expose approved artifact patch application as a Codebox-owned primitive. Homeboy Extensions already delegates preflight and apply request creation when runtime-core exports are available; the remaining local code maps Homeboy worktree, commit, and publish policy around git apply.',
  },
  {
    id: 'preview-materialization',
    schema: RUNTIME_CONTRACT_SCHEMAS.runtimeBoundary.browserContainedSiteOpen,
    owner: 'wp-codebox',
    adapter_behavior: 'delegate_contained_site_open_without_constructing_playground_urls',
    requirement: 'Materialize or open a browser contained-site preview from caller domain inputs and return typed preview evidence including URL, lease, boot, and status metadata when available. Homeboy must not construct Playground URLs downstream.',
  },
];

function wpCodeboxProviderRuntimeInvocationContract() {
  return {
    schema: WP_CODEBOX_PROVIDER_RUNTIME_INVOCATION_CONTRACT_SCHEMA,
    version: 1,
    tasks: { ...WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES },
    abilities: { ...WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES },
    result_schemas: { ...WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS },
  };
}

function wpCodeboxProviderRuntimeOperationEntry(operation, fallbackKey = '') {
  const rawName = typeof operation === 'string'
    ? operation
    : firstValue(operation?.key, operation?.operation, operation?.task, operation?.ability, fallbackKey);
  const key = WP_CODEBOX_PROVIDER_RUNTIME_OPERATION_ALIASES[rawName] || WP_CODEBOX_PROVIDER_RUNTIME_OPERATION_ALIASES[fallbackKey];
  if (!key) {
    return null;
  }
  return [key, wpCodeboxProviderRuntimeOperationConfig(key, operation)];
}

function wpCodeboxProviderRuntimeOperationConfig(key, operation) {
  const resultSchemaKey = WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMA_KEYS[key];
  const explicitConfig = operation && typeof operation === 'object' && !Array.isArray(operation)
    ? firstObject(operation.config, operation.input, operation.args) || {}
    : {};
  return withoutUndefinedValues({
    task: WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES[key],
    ability: WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES[key],
    result_schema: WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS[resultSchemaKey],
    config: Object.keys(explicitConfig).length > 0 ? explicitConfig : undefined,
  });
}

function wpCodeboxAgentFanoutAdapterContract() {
  return {
    schema: 'homeboy-extensions/wp-codebox-agent-fanout-adapter/v1',
    canonical_path: 'homeboy-durable-scheduler-to-homeboy-extensions-codebox-executor-to-wp-codebox-sandbox-fanout',
    ownership: {
      substrate: 'agents-api',
      durable_scheduler: 'homeboy',
      executor_adapter: 'homeboy-extensions',
      sandbox_worker_runtime: 'wp-codebox',
    },
    accepted_request_schema: WP_CODEBOX_AGENT_FANOUT_REQUEST_SCHEMA,
    maps: [
      'task_id',
      'parent_plan_id',
      'group_key',
      'metadata',
      'workspace',
      'artifact_declarations',
      'expected_artifacts',
      'secret_env_names',
      'provider',
      'model',
      'progress_callbacks',
      'evidence_refs',
    ],
  };
}

function firstValue(...values) {
  return values.find((value) => value !== undefined && value !== null && value !== '');
}

function firstObject(...values) {
  return values.find((value) => value && typeof value === 'object' && !Array.isArray(value));
}

function withoutUndefinedValues(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
  WP_CODEBOX_BACKEND,
  WP_CODEBOX_AGENT_FANOUT_REQUEST_SCHEMA,
  WP_CODEBOX_PROVIDER_ID,
  WP_CODEBOX_PROVIDER_LABEL,
  WP_CODEBOX_PROVIDER_CREDENTIAL_BOUNDARY_SCHEMA,
  WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES,
  WP_CODEBOX_PROVIDER_RUNTIME_INVOCATION_CONTRACT_SCHEMA,
  WP_CODEBOX_PROVIDER_RUNTIME_OPERATION_ALIASES,
  WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS,
  WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES,
  WP_CODEBOX_RECIPE_RUN_CLI_COMMAND,
  WP_CODEBOX_ROLE_ALIASES,
  WP_CODEBOX_TASK_REQUEST_SCHEMA,
  WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS,
  WP_CODEBOX_WORKSPACE_MOUNT_KIND,
  wpCodeboxProviderRuntimeInvocationContract,
  wpCodeboxAgentFanoutAdapterContract,
  wpCodeboxProviderRuntimeOperationConfig,
  wpCodeboxProviderRuntimeOperationEntry,
};
