'use strict';

const WP_CODEBOX_TASK_REQUEST_SCHEMA = 'wp-codebox/task-input/v1';
const WP_CODEBOX_PROVIDER_ID = 'wordpress.codebox-agent-task-executor';
const WP_CODEBOX_PROVIDER_LABEL = 'WP Codebox agent task executor';
const WP_CODEBOX_BACKEND = 'codebox';
const WP_CODEBOX_PROVIDER_RUNTIME_INVOCATION_CONTRACT_SCHEMA = 'wp-codebox/provider-runtime-invocation-contract/v1';

const WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES = {
  workspaceCapture: 'wp-codebox.runner-workspace.capture',
  workspaceCommand: 'wp-codebox.runner-workspace.command',
  workspacePublish: 'wp-codebox.runner-workspace.publish',
  toolCallTranscriptRecord: 'wp-codebox.tool-call-transcript.record',
  artifactHandoff: 'wp-codebox.artifact-handoff',
};

const WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES = {
  workspaceCapture: 'wp-codebox/runner-workspace-capture',
  workspaceCommand: 'wp-codebox/runner-workspace-command',
  workspacePublish: 'wp-codebox/runner-workspace-publish',
  toolCallTranscriptRecord: 'wp-codebox/record-tool-call-transcript',
  artifactHandoff: 'wp-codebox/handoff-artifacts',
};

const WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS = {
  workspace_capture: 'wp-codebox/runner-workspace-capture-result/v1',
  workspace_command: 'wp-codebox/runner-workspace-command-result/v1',
  workspace_publication: 'wp-codebox/runner-workspace-publication-result/v1',
  tool_call_transcript: 'wp-codebox/tool-call-transcript/v1',
  evidence_artifact_envelope: 'wp-codebox/evidence-artifact-envelope/v1',
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
    id: 'runtime-profile',
    schema: 'wp-codebox/runtime-profile/v1',
    owner: 'wp-codebox',
    adapter_behavior: 'forward_profile_payload',
    requirement: 'Consume generic runtime dependencies, provider plugins, overlays, env, and mounts without Homeboy expanding Codebox orchestration internals.',
  },
  {
    id: 'parent-tool-bridge',
    schema: 'wp-codebox/parent-tool-bridge/v1',
    owner: 'wp-codebox',
    adapter_behavior: 'declare_requirement_when_missing',
    requirement: 'Expose parent-owned tools inside the sandbox through a Codebox-owned bridge component instead of Homeboy injecting bridge implementation details.',
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
    schema: WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS.evidence_artifact_envelope,
    owner: 'wp-codebox',
    adapter_behavior: 'consume_when_available',
    requirement: 'Return typed artifacts, evidence refs, and run summaries in stable envelopes so adapters do not parse backend-local artifact layouts. Until this is complete, compatibility is centralized in codebox-artifact-contract.js.',
  },
  {
    id: 'artifact-apply-execution',
    schema: 'wp-codebox/artifact-apply-result/v1',
    owner: 'wp-codebox',
    adapter_behavior: 'local_git_apply_until_primitive_exists',
    requirement: 'Expose approved artifact patch application as a Codebox-owned primitive. Homeboy Extensions already delegates preflight and apply request creation when runtime-core exports are available; the remaining local code maps Homeboy worktree, commit, and publish policy around git apply.',
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
  WP_CODEBOX_PROVIDER_ID,
  WP_CODEBOX_PROVIDER_LABEL,
  WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES,
  WP_CODEBOX_PROVIDER_RUNTIME_INVOCATION_CONTRACT_SCHEMA,
  WP_CODEBOX_PROVIDER_RUNTIME_OPERATION_ALIASES,
  WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS,
  WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES,
  WP_CODEBOX_ROLE_ALIASES,
  WP_CODEBOX_TASK_REQUEST_SCHEMA,
  WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS,
  wpCodeboxProviderRuntimeInvocationContract,
  wpCodeboxProviderRuntimeOperationConfig,
  wpCodeboxProviderRuntimeOperationEntry,
};
