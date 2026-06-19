'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  codeboxTaskRequestFromAgentTaskRequest,
  normalizeCodeboxArtifactDeclaration,
  normalizeCodeboxArtifactOutcome,
  providerContract,
  providerRuntimeInvocationContract,
  typedArtifactsFromCodeboxResult,
} = require('../../agent-runtimes/wp-codebox');

const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wordpress-agent-boundary-'));
const codexSecretEnv = [
  'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
  'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
  'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
];
const claudeCodeSecretEnv = [
  'AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN',
  'AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN',
  'AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT',
];

function restoreEnv(name, value) {
  if (value === undefined) {
    delete process.env[name];
    return;
  }
  process.env[name] = value;
}

function secretEnvRequirementForProvider(contract, provider) {
  return contract.secret_env_requirements.find((requirement) => (
    requirement.when.any.some((selector) => selector.path === 'executor.config.provider' && selector.equals === provider)
  ));
}

const provider = providerContract();
assert.equal(provider.id, 'wordpress.codebox-agent-task-executor');
assert.equal(provider.label, 'WP Codebox agent task executor');
assert.equal(provider.backend, 'codebox');
assert.equal(provider.runtime_id, 'wp-codebox');
assert.equal(provider.integration_contract, 'homeboy-wordpress-agent-task/v1');
assert.deepEqual(provider.provider_runtime_invocation, providerRuntimeInvocationContract());
assert.equal(provider.provider_runtime_invocation.tasks.workspaceCommand, 'wp-codebox.runner-workspace.command');
assert.equal(provider.provider_runtime_invocation.abilities.workspaceCommand, 'wp-codebox/runner-workspace-command');
assert.equal(provider.upstream_primitive_requirements.some((requirement) => requirement.id === 'artifact-apply-execution'), true);
assert.equal(
  provider.upstream_primitive_requirements.find((requirement) => requirement.id === 'artifact-result-envelope').adapter_behavior,
  'consume_when_available'
);
assert.doesNotMatch(JSON.stringify(provider.provider_runtime_invocation), /datamachine|data machine|wp-site-generator|wpsg|site generator/i);
assert.deepEqual(secretEnvRequirementForProvider(provider, 'codex').env, codexSecretEnv);

assert.deepEqual(normalizeCodeboxArtifactDeclaration('fallback', {
  schema: 'homeboy/agent-task-artifact-declaration/v1',
  id: 'review-report',
  artifactSchema: 'example/report/v1',
}), {
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'review-report',
  artifact_schema: 'example/report/v1',
  required: true,
});
assert.deepEqual(typedArtifactsFromCodeboxResult({
  metadata: {
    agent_runtime: {
      result: {
        outputs: {
          typed_artifacts: {
            review: { type: 'json', payload: { ok: true } },
          },
        },
      },
    },
  },
}).review.payload, { ok: true });
assert.equal(normalizeCodeboxArtifactOutcome({ id: 'patch.diff', kind: 'codebox-patch' }, {}, {
  roleAliases: provider.role_aliases,
}).role, 'patch');

const manifest = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'wordpress.json'), 'utf8'));
assert.equal(manifest.agent_task_executors, undefined);
assert.equal(manifest.agent_runtimes, undefined);
assert.equal(manifest.agent_task.default_backend, undefined);
assert.equal(manifest.agent_task.runtime_requirements.integration_contract, 'homeboy-wordpress-agent-task/v1');
const runtime = JSON.parse(fs.readFileSync(path.join(__dirname, '..', '..', 'agent-runtimes', 'wp-codebox', 'wp-codebox.json'), 'utf8'));
assert.equal(runtime.agent_task_executors.length, 1);
assert.deepEqual(runtime.agent_task_executors[0], providerContract());
assert.deepEqual(runtime.agent_task_executors[0].provider_runtime_invocation, providerRuntimeInvocationContract());
assert.deepEqual(provider.runner_readiness, [{
  id: 'wp-codebox.executable',
  label: 'WP Codebox executable',
  executable: {
    env: ['HOMEBOY_WP_CODEBOX_BIN'],
    candidates: ['wp-codebox'],
    version_command: ['--version'],
    install_hint: 'Install WP Codebox or set the generic runtime_bin executor config; HOMEBOY_WP_CODEBOX_BIN remains a legacy compatibility env alias.',
  },
  remediation: 'Install WP Codebox or set the generic runtime_bin executor config; HOMEBOY_WP_CODEBOX_BIN remains a legacy compatibility env alias.',
}]);
assert.deepEqual(secretEnvRequirementForProvider(runtime.agent_task_executors[0], 'codex').env, codexSecretEnv);
assert.equal(provider.capabilities.includes('tool:wpsg_materialize_packet'), false);
assert.equal(provider.capabilities.includes('ability:wpsg_materialize_packet'), false);
assert.deepEqual(provider.provider_defaults.openai.secret_env, ['OPENAI_API_KEY']);
assert.deepEqual(provider.provider_defaults.codex.secret_env, [
  'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
  'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
  'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
]);
assert.deepEqual(provider.provider_defaults.codex.secret_env_sources.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN, {
  source: 'json-file',
  path: '~/.codex/auth.json',
  field: 'tokens.access_token',
});
assert.deepEqual(provider.provider_defaults['claude-code'].secret_env, claudeCodeSecretEnv);
assert.deepEqual(provider.provider_defaults['claude-code'].secret_env_sources.AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN, {
  source: 'json-file',
  path: '~/.local/share/opencode/auth.json',
  field: 'anthropic.refresh',
});
assert.deepEqual(provider.workspace_tools.readonly, [
  'workspace_ls',
  'workspace_read',
  'workspace_git_status',
]);
assert.deepEqual(provider.component_path_defaults, {});
assert.equal(provider.capabilities.includes('tool:example/run-agent-bundle'), false);
assert.equal(provider.capabilities.includes('ability:example/run-agent-bundle'), false);

const customContract = providerContract({
  capabilities: ['wordpress_sandbox', 'tool:example/run-workflow'],
  workspaceTools: {
    readonly: ['example_workspace_read'],
    readwrite: ['example_workspace_write'],
  },
  componentPathDefaults: {
    contract_slug_map: { 'example-runtime': 'agent_runtime' },
    path_aliases: { agent_runtime: ['runtime_component:example_runtime'] },
  },
});
assert.equal(customContract.runtime_id, 'wp-codebox');
assert.deepEqual(customContract.capabilities, ['wordpress_sandbox', 'tool:example/run-workflow']);
assert.deepEqual(customContract.workspace_tools.readwrite, ['example_workspace_write']);
assert.deepEqual(customContract.component_path_defaults.contract_slug_map, { 'example-runtime': 'agent_runtime' });

const taskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'generic-wordpress-task-1',
  executor: {
    backend: 'codebox',
    config: { provider: 'openai' },
  },
  instructions: 'Run a generic WordPress ability with declared tools.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
  tools: [
    'wordpress.read-post',
    { id: 'wordpress.write-post' },
  ],
  abilities: [
    { name: 'wordpress/site-health' },
  ],
  inputs: {
    ability_request: {
      name: 'wordpress/site-health',
      input: { include_debug: false },
    },
  },
});

assert.equal(taskInput.schema, 'wp-codebox/task-input/v1');
assert.equal(taskInput.parent_request.executor.backend, 'codebox');
assert.equal(Object.hasOwn(taskInput, 'agent'), false);
assert.deepEqual(taskInput.runtime_task, {
  ability: 'wordpress/site-health',
  input: { include_debug: false },
});
assert.deepEqual(
  taskInput.allowed_tools.filter((tool) => tool.startsWith('wordpress.')),
  ['wordpress.read-post', 'wordpress.write-post']
);
assert.equal(taskInput.allowed_tools.includes('wordpress/site-health'), true);
assert.equal(taskInput.allowed_tools.includes('workspace_apply_patch'), true);
assert.equal(taskInput.sandbox_tool_policy.schema, 'wp-codebox/sandbox-tool-policy/v1');
assert.equal(
  taskInput.sandbox_tool_policy.tools.some((tool) => tool.id === 'wordpress.read-post'),
  true
);

const originalToolPolicyEnv = process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON;
const originalToolRequestSchemaEnv = process.env.HOMEBOY_AGENT_TOOL_REQUEST_SCHEMA;
const originalToolResultSchemaEnv = process.env.HOMEBOY_AGENT_TOOL_RESULT_SCHEMA;
const originalToolPolicySchemaEnv = process.env.HOMEBOY_AGENT_TOOL_POLICY_SCHEMA;
const homeboyAgentToolPolicyJson = JSON.stringify({
  schema: 'homeboy/agent-tool-policy/v1',
  default_location: 'disabled',
  tools: {
    workspace_read: { execution_location: 'runner', reason: 'safe in sandbox' },
    github_issue_publish: { execution_location: 'control_plane', timeout_ms: 30000, reason: 'host owns GitHub credentials' },
    github_pull_request_publish: { execution_location: 'disabled', reason: 'not needed in this run' },
  },
});
process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON = homeboyAgentToolPolicyJson;
process.env.HOMEBOY_AGENT_TOOL_REQUEST_SCHEMA = 'homeboy/agent-tool-request/v1';
process.env.HOMEBOY_AGENT_TOOL_RESULT_SCHEMA = 'homeboy/agent-tool-result/v1';
process.env.HOMEBOY_AGENT_TOOL_POLICY_SCHEMA = 'homeboy/agent-tool-policy/v1';
const homeboyToolPolicyTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'homeboy-tool-policy-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Run with host-owned GitHub tools.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
  tools: ['workspace_read', 'github_issue_publish', 'github_pull_request_publish'],
});
restoreEnv('HOMEBOY_AGENT_TOOL_POLICY_JSON', originalToolPolicyEnv);
restoreEnv('HOMEBOY_AGENT_TOOL_REQUEST_SCHEMA', originalToolRequestSchemaEnv);
restoreEnv('HOMEBOY_AGENT_TOOL_RESULT_SCHEMA', originalToolResultSchemaEnv);
restoreEnv('HOMEBOY_AGENT_TOOL_POLICY_SCHEMA', originalToolPolicySchemaEnv);

assert.equal(homeboyToolPolicyTaskInput.allowed_tools.includes('workspace_read'), true);
assert.equal(homeboyToolPolicyTaskInput.allowed_tools.includes('github_issue_publish'), false);
assert.equal(homeboyToolPolicyTaskInput.allowed_tools.includes('github_pull_request_publish'), false);
assert.equal(homeboyToolPolicyTaskInput.runtime_env.HOMEBOY_AGENT_TOOL_REQUEST_SCHEMA, undefined);
assert.equal(homeboyToolPolicyTaskInput.runtime_env.HOMEBOY_AGENT_TOOL_POLICY_JSON, undefined);
assert.equal(homeboyToolPolicyTaskInput.runtime_env.DATAMACHINE_HOST_TOOL_POLICY_JSON, undefined);
assert.equal(homeboyToolPolicyTaskInput.sandbox_tool_policy.metadata.source, 'homeboy.codebox-agent-task.default-workspace-tools');
assert.equal(homeboyToolPolicyTaskInput.sandbox_tool_policy.tools.some((tool) => tool.id === 'github_issue_publish'), false);

const codeboxOwnedBridgeTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'codebox-owned-parent-tool-bridge-task-1',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'openai',
      runtime_profile: {
        schema: 'wp-codebox/runtime-profile/v1',
        id: 'codebox-owned-bridge',
        parent_tool_bridge: {
          schema: 'wp-codebox/parent-tool-bridge/v1',
        },
      },
    },
  },
  instructions: 'Run with a Codebox-owned parent tool bridge.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
  tools: ['workspace_read'],
});
assert.equal(codeboxOwnedBridgeTaskInput.runtime_requirements.parent_tool_bridge.schema, 'wp-codebox/parent-tool-bridge/v1');
assert.equal(codeboxOwnedBridgeTaskInput.runtime_requirements.upstream_primitive_requirements, undefined);

const runtimeProfileDependencyTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'runtime-profile-dependencies-task-1',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'openai',
      runtime_requirements: {
        components: [{ slug: 'runtime-component', path: '/components/runtime-component' }],
        plugins: [{ slug: 'runtime-plugin', path: '/plugins/runtime-plugin' }],
      },
    },
  },
  instructions: 'Forward Codebox-owned runtime profile dependencies.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
});
assert.deepEqual(runtimeProfileDependencyTaskInput.runtime_requirements.components, [{ slug: 'runtime-component', path: '/components/runtime-component' }]);
assert.deepEqual(runtimeProfileDependencyTaskInput.runtime_requirements.plugins, [{ slug: 'runtime-plugin', path: '/plugins/runtime-plugin' }]);
assert.deepEqual(runtimeProfileDependencyTaskInput.runtime_requirements.component_contracts.map((contract) => ({
  slug: contract.slug,
  path: contract.path,
  loadAs: contract.loadAs,
  activate: contract.activate,
})), [
  { slug: 'runtime-component', path: '/components/runtime-component', loadAs: 'mu-plugin', activate: false },
  { slug: 'runtime-plugin', path: '/plugins/runtime-plugin', loadAs: 'plugin', activate: true },
]);
assert.deepEqual(runtimeProfileDependencyTaskInput.runtime_requirements.extra_plugins.map((plugin) => plugin.slug), ['runtime-component', 'runtime-plugin']);
assert.deepEqual(runtimeProfileDependencyTaskInput.component_contracts.map((contract) => contract.slug), ['runtime-component', 'runtime-plugin']);
assert.equal(runtimeProfileDependencyTaskInput.runtime_requirements.upstream_primitive_requirements[0].id, 'parent-tool-bridge');
assert.equal(runtimeProfileDependencyTaskInput.runtime_requirements.upstream_primitive_requirements[0].adapter_behavior, 'declare_requirement_only');

const customRuntimePolicyTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'custom-runtime-policy-task-1',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'openai',
      component_contracts: [{ slug: 'example-runtime', path: '/components/example-runtime' }],
      runtime_components: { example_tools: '/components/example-tools' },
    },
  },
  instructions: 'Run against caller-declared runtime defaults.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
}, {
  workspaceTools: {
    readonly: ['example_workspace_read'],
    readwrite: ['example_workspace_write'],
  },
  componentPathDefaults: {
    contract_slug_map: { 'example-runtime': 'agent_runtime' },
    path_aliases: {
      agent_runtime: ['contract:agent_runtime'],
      agent_runtime_tools: ['runtime_component:example_tools'],
    },
  },
});
assert.deepEqual(customRuntimePolicyTaskInput.allowed_tools, [
  'example_workspace_read',
  'example_workspace_write',
]);
assert.equal(customRuntimePolicyTaskInput.runtime_component_paths.agent_runtime, '/components/example-runtime');
assert.equal(customRuntimePolicyTaskInput.runtime_component_paths.agent_runtime_tools, '/components/example-tools');

const runtimeInvocationTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'generic-provider-runtime-invocation-task-1',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'codex',
      provider_runtime_invocation: {
        operations: {
          workspaceCommand: { config: { timeout_ms: 30000 } },
          artifactHandoff: true,
          'wp-codebox/runner-workspace-capture': {},
        },
      },
    },
  },
  instructions: 'Run with generic WP Codebox provider runtime invocation names.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
});
assert.equal(runtimeInvocationTaskInput.provider_runtime_invocation.schema, 'wp-codebox/provider-runtime-invocation-contract/v1');
assert.deepEqual(runtimeInvocationTaskInput.provider_runtime_invocation.operations.workspaceCommand, {
  task: 'wp-codebox.runner-workspace.command',
  ability: 'wp-codebox/runner-workspace-command',
  result_schema: 'wp-codebox/runner-workspace-command-result/v1',
  config: { timeout_ms: 30000 },
});
assert.equal(runtimeInvocationTaskInput.provider_runtime_invocation.operations.artifactHandoff.ability, 'wp-codebox/handoff-artifacts');
assert.equal(runtimeInvocationTaskInput.provider_runtime_invocation.operations.workspaceCapture.task, 'wp-codebox.runner-workspace.capture');
assert.doesNotMatch(JSON.stringify(runtimeInvocationTaskInput.provider_runtime_invocation), /datamachine|data machine|wp-site-generator|wpsg|site generator/i);

const customRuntimeProfileTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'custom-runtime-profile-task-1',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'openai',
      runtime_profile: 'example-runtime-profile',
      runtime_profiles: {
        'example-runtime-profile': {
          schema: 'homeboy/runtime-profile/v1',
          id: 'example-runtime-profile',
          workspace_tools: {
            readonly: ['profile_workspace_read'],
            readwrite: ['profile_workspace_write'],
          },
          component_path_defaults: {
            contract_slug_map: { 'example-profile-runtime': 'agent_runtime' },
            path_aliases: { agent_runtime: ['contract:agent_runtime'] },
          },
          ability_requirements: ['example/run-profile-workflow'],
        },
      },
      component_contracts: [{ slug: 'example-profile-runtime', path: '/components/example-profile-runtime' }],
    },
  },
  instructions: 'Run against a named runtime profile.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
});

assert.deepEqual(customRuntimeProfileTaskInput.allowed_tools, [
  'profile_workspace_read',
  'profile_workspace_write',
  'example/run-profile-workflow',
]);
assert.equal(customRuntimeProfileTaskInput.runtime_component_paths.agent_runtime, '/components/example-profile-runtime');

const repoLoopBundleTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'repo-loop-agent-bundle-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Run a repo-loop bundle workflow.',
  artifacts: {
    outputs: {
      concept_packet: {
        type: 'ConceptPacket',
        schema: 'example/concept-packet/v1',
        required: true,
      },
    },
  },
  inputs: {
    ability_request: { name: 'example/run-agent-bundle' },
    runtime_input_mapping: [
      { from: 'client_context.inputs.source', to: 'source' },
      { from: 'client_context.inputs.flow', to: 'flow' },
      { from: 'client_context.inputs.wait_for_completion', to: 'wait_for_completion' },
    ],
    client_context: {
      inputs: {
        source: 'bundles/example-agent',
        flow: 'example-artifact-flow',
        wait_for_completion: true,
      },
    },
  },
});

assert.equal(repoLoopBundleTaskInput.runtime_task.ability, 'example/run-agent-bundle');
assert.deepEqual(repoLoopBundleTaskInput.runtime_task.input, {
  source: 'bundles/example-agent',
  flow: 'example-artifact-flow',
  wait_for_completion: true,
});
assert.deepEqual(repoLoopBundleTaskInput.artifact_declarations, [{
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'concept_packet',
  type: 'ConceptPacket',
  artifact_schema: 'example/concept-packet/v1',
  required: true,
}]);

const controllerClientContextArtifactsTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'controller-client-context-artifacts-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Run a controller workflow with domain artifact declarations in client context.',
  dispatch: {
    client_context: JSON.stringify({
      artifacts: [{
        artifact_id: 'concept_packet',
        kind: 'wp-site-generator/ConceptPacket/v1',
        required: true,
      }],
    }),
  },
  inputs: {
    ability_request: { name: 'agents/run-runtime-package' },
  },
});

assert.deepEqual(controllerClientContextArtifactsTaskInput.artifact_declarations, [{
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'concept_packet',
  artifact_schema: 'wp-site-generator/ConceptPacket/v1',
  required: true,
}]);
assert.deepEqual(controllerClientContextArtifactsTaskInput.runtime_task.input.required_artifacts, ['concept_packet']);
assert.deepEqual(controllerClientContextArtifactsTaskInput.runtime_task.input.engine_data_outputs, {
  concept_packet: 'metadata.engine_data.outputs.typed_artifacts.concept_packet.payload',
});

const providerAndControllerArtifactsTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'provider-and-controller-artifacts-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Run a controller workflow with provider and domain artifact declarations.',
  artifact_declarations: [{
    name: 'patch',
    required: true,
  }],
  dispatch: {
    client_context: JSON.stringify({
      artifacts: [{
        artifact_id: 'concept_packet',
        kind: 'wp-site-generator/ConceptPacket/v1',
        required: true,
      }],
    }),
  },
  inputs: {
    ability_request: { name: 'agents/run-runtime-package' },
  },
});
assert.deepEqual(providerAndControllerArtifactsTaskInput.artifact_declarations.map((declaration) => declaration.name), ['patch', 'concept_packet']);
assert.deepEqual(providerAndControllerArtifactsTaskInput.runtime_task.input.required_artifacts, ['patch', 'concept_packet']);
assert.equal(
  providerAndControllerArtifactsTaskInput.runtime_task.input.engine_data_outputs.concept_packet,
  'metadata.engine_data.outputs.typed_artifacts.concept_packet.payload'
);

const genericRepoLoopTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'repo-loop-generic-ability-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Run a generic declared ability with workflow inputs.',
  client_context: {
    inputs: {
      packet: 'artifact-42',
      dry_run: true,
    },
  },
  inputs: {
    ability_request: {
      name: 'example/materialize-artifact',
      input_defaults: { dry_run: true, format: 'json' },
      input_mapping: [
        { from: 'client_context.inputs.packet', to: 'packet' },
        { from: 'client_context.inputs.dry_run', to: 'dry_run' },
      ],
      input: { dry_run: false },
    },
    context: {
      inputs: { secret: 'not-forwarded-from-ambient-context' },
    },
  },
});

assert.equal(genericRepoLoopTaskInput.runtime_task.ability, 'example/materialize-artifact');
assert.deepEqual(genericRepoLoopTaskInput.runtime_task.input, {
  packet: 'artifact-42',
  format: 'json',
  dry_run: false,
});

const noImplicitClientContextTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'generic-ability-no-ambient-client-context-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Run a generic declared ability without ambient client context merge.',
  client_context: {
    inputs: {
      packet: 'ambient-artifact',
      dry_run: true,
    },
  },
  inputs: {
    ability_request: {
      name: 'example/materialize-artifact',
      input: { explicit: true },
    },
  },
});
assert.deepEqual(noImplicitClientContextTaskInput.runtime_task.input, { explicit: true });

const legacyClientContextTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'generic-ability-legacy-client-context-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Run a queued legacy declared ability with explicit legacy context merge.',
  client_context: {
    inputs: {
      packet: 'legacy-artifact',
      dry_run: true,
    },
  },
  inputs: {
    allow_legacy_client_context_input_merge: true,
    ability_request: {
      name: 'example/materialize-artifact',
      input: { dry_run: false },
    },
  },
});
assert.deepEqual(legacyClientContextTaskInput.runtime_task.input, {
  packet: 'legacy-artifact',
  dry_run: false,
});

const repoLoopWorkspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wordpress-repo-loop-workspace-'));
const repoLoopWorkspaceTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'repo-loop-workspace-task-1',
  cwd: repoLoopWorkspaceRoot,
  repo: 'example-repo@example-loop-main-20260616',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'openai',
      workspace_required: true,
    },
  },
  instructions: 'Run a repo-loop workflow against the current checkout.',
  inputs: {
    ability_request: { name: 'example/run-agent-bundle' },
  },
});

const repoLoopWorkspaceMount = repoLoopWorkspaceTaskInput.mounts.find(
  (mount) => mount.metadata?.kind === 'homeboy-dmc-workspace'
);
assert(repoLoopWorkspaceMount, 'repo-loop cwd is translated into a Codebox workspace mount');
assert.equal(repoLoopWorkspaceMount.source, repoLoopWorkspaceRoot);
assert.equal(repoLoopWorkspaceMount.target, `/workspace/${path.basename(repoLoopWorkspaceRoot)}`);
assert.equal(repoLoopWorkspaceMount.mode, 'readwrite');
assert.equal(repoLoopWorkspaceTaskInput.allowed_tools.includes('workspace_apply_patch'), true);
assert.deepEqual(repoLoopWorkspaceTaskInput.workspace_materialization, {
  repo: 'example-repo@example-loop-main-20260616',
  cwd: repoLoopWorkspaceRoot,
  root: repoLoopWorkspaceRoot,
});
assert.deepEqual(
  repoLoopWorkspaceTaskInput.target.materialization,
  repoLoopWorkspaceTaskInput.workspace_materialization
);

const repoLoopTypedOutputsTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'repo-loop-typed-outputs-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Run a repo-loop step that declares typed outputs generically.',
  outputs: {
    typed_artifacts: [{
      name: 'concept_packet',
      type: 'ConceptPacket',
      artifact_schema: 'example/concept-packet/v1',
    }],
  },
  inputs: {
    ability_request: { name: 'example/run-agent-bundle' },
  },
});

assert.deepEqual(repoLoopTypedOutputsTaskInput.artifact_declarations, [{
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'concept_packet',
  type: 'ConceptPacket',
  artifact_schema: 'example/concept-packet/v1',
  required: true,
}]);

const legacyTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'legacy-codebox-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Already queued legacy request.',
  inputs: {},
});

assert.equal(legacyTaskInput.schema, 'wp-codebox/task-input/v1');

const explicitAgentTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'explicit-agent-codebox-task-1',
  executor: {
    backend: 'codebox',
    config: { provider: 'openai', agent: 'custom-sandbox-agent' },
  },
  instructions: 'Run with an explicitly selected sandbox agent.',
  inputs: {},
});

assert.equal(explicitAgentTaskInput.agent, 'custom-sandbox-agent');

const previousHomeboySettingsJson = process.env.HOMEBOY_SETTINGS_JSON;
process.env.HOMEBOY_SETTINGS_JSON = JSON.stringify({
  provider_plugin_paths: ['/missing/stale-openai-provider'],
});
let codexTaskInput;
try {
  codexTaskInput = codeboxTaskRequestFromAgentTaskRequest({
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'codex-codebox-task-1',
    executor: { backend: 'codebox', config: { provider: 'codex' } },
    instructions: 'Run a Codex-backed Codebox task.',
    inputs: {},
  });
} finally {
  if (previousHomeboySettingsJson === undefined) {
    delete process.env.HOMEBOY_SETTINGS_JSON;
  } else {
    process.env.HOMEBOY_SETTINGS_JSON = previousHomeboySettingsJson;
  }
}
assert.deepEqual(codexTaskInput.provider_plugin_paths, ['/missing/stale-openai-provider']);
assert.deepEqual(codexTaskInput.secret_env, provider.provider_defaults.codex.secret_env);

const previousSecretEnvPlan = process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON;
process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON = JSON.stringify({
  schema: 'homeboy/secret-env-plan/v1',
  secret_env_names: ['HOMEBOY_PLANNED_CODEBOX_SECRET'],
  env_name_mapping: {
    'wordpress.codebox-agent-task-executor': ['HOMEBOY_PLANNED_CODEBOX_SECRET'],
  },
  status: [{ name: 'HOMEBOY_PLANNED_CODEBOX_SECRET', configured: true, source: 'env' }],
});
let plannedSecretEnvTaskInput;
try {
  plannedSecretEnvTaskInput = codeboxTaskRequestFromAgentTaskRequest({
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'planned-secret-env-codebox-task-1',
    executor: {
      backend: 'codebox',
      config: { provider: 'codex', secret_env: ['EXPLICIT_CODEBOX_SECRET'] },
    },
    instructions: 'Run a Codex-backed Codebox task with Homeboy-planned secret env.',
    inputs: {},
  });
} finally {
  if (previousSecretEnvPlan === undefined) {
    delete process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON;
  } else {
    process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON = previousSecretEnvPlan;
  }
}
assert.deepEqual(plannedSecretEnvTaskInput.secret_env, [
  'HOMEBOY_PLANNED_CODEBOX_SECRET',
  'EXPLICIT_CODEBOX_SECRET',
]);

const claudeCodeTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'claude-code-codebox-task-1',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'claude-code',
      model: 'opus-4.7',
    },
  },
  instructions: 'Run a Claude Code backed Codebox task.',
  inputs: {},
});
assert.deepEqual(claudeCodeTaskInput.secret_env, provider.provider_defaults['claude-code'].secret_env);

const providerRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-provider-'));
const providerPath = path.join(providerRoot, 'provider-plugin');
fs.mkdirSync(providerPath, { recursive: true });
process.env.HOMEBOY_SETTINGS_JSON = JSON.stringify({
  provider_plugin_paths: { codex: [providerPath] },
});
let configuredCodexTaskInput;
try {
  configuredCodexTaskInput = codeboxTaskRequestFromAgentTaskRequest({
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'configured-codex-codebox-task-1',
    executor: { backend: 'codebox', config: { provider: 'codex' } },
    instructions: 'Run a Codex-backed Codebox task with the configured provider checkout.',
    inputs: {},
  });
} finally {
  if (previousHomeboySettingsJson === undefined) {
    delete process.env.HOMEBOY_SETTINGS_JSON;
  } else {
    process.env.HOMEBOY_SETTINGS_JSON = previousHomeboySettingsJson;
  }
}
assert.deepEqual(configuredCodexTaskInput.provider_plugin_paths, [providerPath]);

console.log('Codebox agent-task executor boundary contract passed');
