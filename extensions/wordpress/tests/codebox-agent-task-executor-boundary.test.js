'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(__dirname, '..', '..', 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');

const {
  agentTaskOutcomeFromCodeboxResult,
  artifactResultEnvelopeFromCodeboxResult,
  codeboxFanoutRequestFromAgentTaskRequest,
  codeboxTaskRequestFromAgentTaskRequest,
  codeboxRunAgentTaskInvocation,
  normalizeCodeboxAgentTaskEvents,
  normalizeCodeboxPublicResultEnvelope,
  normalizeCodeboxArtifactDeclaration,
  normalizeCodeboxArtifactOutcome,
  publicEnvelopeBoundaryDiagnostic,
  providerContract,
  providerRuntimeInvocationContract,
  wpCodeboxAgentFanoutAdapterContract,
  reconcileRunSummaryWithPublicEnvelope,
  runtimeContractSchemas,
  typedArtifactsFromCodeboxResult,
} = require('../../agent-runtimes/wp-codebox');
const runtimeAgentCi = require('../../runtime-agent-ci/provider-adapters');

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

function validateSecretEnvPlan(plan) {
  const fixturePath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-secret-env-plan-')), 'plan.json');
  fs.writeFileSync(fixturePath, `${JSON.stringify(plan, null, 2)}\n`);
  const result = spawnSync(process.env.HOMEBOY_COMMAND || 'homeboy', [
    'contract',
    'validate',
    runtimeAgentCi.SECRET_ENV_PLAN_SCHEMA,
    '--file',
    fixturePath,
  ], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const output = JSON.parse(result.stdout);
  assert.equal(output.success, true);
  assert.equal(output.data.valid, true);
}

function secretEnvRequirementForProvider(contract, provider) {
  return contract.secret_env_requirements.find((requirement) => (
    requirement.when.any.some((selector) => selector.path === 'executor.config.provider' && selector.equals === provider)
  ));
}

const provider = providerContract();
assert.equal(provider.id, 'wordpress.codebox-agent-task-executor');
assert.equal(provider.label, 'WP Codebox agent task executor');
assert.equal(provider.backend, 'wp-codebox');
assert.equal(provider.runtime_id, 'wp-codebox');
assert.equal(provider.integration_contract, 'homeboy-wordpress-agent-task/v1');
assert.equal(provider.provider_credential_boundary.schema, 'wp-codebox/provider-credential-boundary/v1');
assert.deepEqual(provider.agent_fanout_adapter, wpCodeboxAgentFanoutAdapterContract());
assert.equal(provider.agent_fanout_adapter.ownership.durable_scheduler, 'homeboy');
assert.equal(provider.agent_fanout_adapter.ownership.executor_adapter, 'homeboy-extensions');
assert.equal(provider.agent_fanout_adapter.ownership.sandbox_worker_runtime, 'wp-codebox');
assert.equal(provider.upstream_primitive_requirements.some((requirement) => requirement.id === 'provider-credential-boundary'), true);
assert.equal(provider.upstream_primitive_requirements.some((requirement) => requirement.id === 'agent-fanout-request' && requirement.schema === 'wp-codebox/agent-fanout-request/v1'), true);
assert.deepEqual(provider.provider_runtime_invocation, providerRuntimeInvocationContract());
assert.equal(provider.provider_runtime_invocation.tasks.workspaceCommand, 'wp-codebox.runner-workspace.command');
assert.equal(provider.provider_runtime_invocation.abilities.workspaceCommand, 'wp-codebox/runner-workspace-command');
assert.equal(provider.upstream_primitive_requirements.some((requirement) => requirement.id === 'artifact-apply-execution'), true);
const runAgentTaskRequirement = provider.upstream_primitive_requirements.find((requirement) => requirement.id === 'run-agent-task');
assert.equal(runAgentTaskRequirement.schema, 'wp-codebox/run-agent-task/v1');
assert.equal(
  provider.upstream_primitive_requirements.find((requirement) => requirement.id === 'artifact-result-envelope').adapter_behavior,
  'consume_canonical_public_envelope_only'
);
assert.doesNotMatch(JSON.stringify(provider.provider_runtime_invocation), /datamachine|data machine|wp-site-generator|wpsg|site generator/i);
assert.deepEqual(secretEnvRequirementForProvider(provider, 'codex').env, codexSecretEnv);
assert.throws(() => codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'raw-provider-credentials-task-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'openai',
      provider_credentials: { OPENAI_API_KEY: 'raw-value' },
    },
  },
  instructions: 'Reject raw provider credential payloads.',
  inputs: {},
}), /provider credential boundary accepts secret_env names only/);

assert.deepEqual(normalizeCodeboxArtifactDeclaration('fallback', {
  schema: 'homeboy/agent-task-artifact-declaration/v1',
  id: 'review-report',
  artifact_schema: 'example/report/v1',
}), {
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'review-report',
  artifact_schema: 'example/report/v1',
  required: true,
});
assert.deepEqual(typedArtifactsFromCodeboxResult({
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    result: {
      outputs: {
        typed_artifacts: {
          review: { type: 'json', payload: { ok: true } },
        },
      },
    },
  },
}).review.payload, { ok: true });
const artifactResultEnvelope = artifactResultEnvelopeFromCodeboxResult({
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    artifact_bundle_refs: [{ kind: 'artifact-bundle', path: 'artifacts/run-1' }],
    artifact_refs: [{ kind: 'codebox-review', path: 'files/review.json' }],
    evidence_refs: [{ kind: 'codebox-agent-terminal-result', uri: 'artifacts/run-1', label: 'Terminal result' }],
    transcript_refs: [{ kind: 'codebox-transcript', path: 'files/transcript.json' }],
    typed_artifacts: [{ name: 'review', artifact_schema: 'example/review/v1', payload: { ok: true } }],
  },
});
assert.equal(artifactResultEnvelope.artifactRefs.length, 2);
assert.equal(artifactResultEnvelope.evidenceRefs.length, 2);
assert.equal(artifactResultEnvelope.evidenceRefs[0].uri, 'artifacts/run-1');
assert.deepEqual(typedArtifactsFromCodeboxResult({ artifact_result: artifactResultEnvelope }).review.payload, { ok: true });
assert.deepEqual(normalizeCodeboxPublicResultEnvelope({ artifact_result: artifactResultEnvelope }).outputs, {});
assert.deepEqual(typedArtifactsFromCodeboxResult({ outputs: { artifact_result: artifactResultEnvelope } }).review.payload, { ok: true });
assert.equal(normalizeCodeboxPublicResultEnvelope({ outputs: { artifact_result: artifactResultEnvelope } }).artifact_result.schema, 'wp-codebox/artifact-result-envelope/v1');
const privateRuntimeShapeRequest = {
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'private-runtime-shape-boundary',
  executor: { backend: 'wp-codebox', config: {} },
  instructions: 'Reject private Codebox runtime result shapes.',
  inputs: {},
};
assert.equal(publicEnvelopeBoundaryDiagnostic({ run: { agentResult: { reply: 'private' } } }).data.required_schema, 'wp-codebox/artifact-result-envelope/v1');
const dispatchIdentityOutcome = agentTaskOutcomeFromCodeboxResult({
  ...privateRuntimeShapeRequest,
  task_id: 'dispatch-identity-boundary',
  inputs: {
    dispatch_identity: {
      source: 'agents-api',
      dispatch_id: 'dispatch-123',
      conversation_id: 'conversation-456',
    },
  },
}, {
  success: true,
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    result: { outputs: { reply: 'Public reply' } },
  },
});
assert.deepEqual(dispatchIdentityOutcome.metadata.dispatch_identity, {
  source: 'agents-api',
  dispatch_id: 'dispatch-123',
  conversation_id: 'conversation-456',
});
const publicRuntimeShapeOutcome = agentTaskOutcomeFromCodeboxResult(privateRuntimeShapeRequest, {
  success: true,
  run: {
    agentResult: {
      reply: 'This private result shape must be ignored.',
    },
  },
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    result: {
      outputs: { reply: 'Public reply' },
    },
  },
});
assert.equal(publicRuntimeShapeOutcome.status, 'succeeded');
assert.equal(publicRuntimeShapeOutcome.outputs.reply, 'Public reply');
assert.equal(publicRuntimeShapeOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.public_result_envelope_missing'), false);
const privateReplyWithPublicEnvelopeOutcome = agentTaskOutcomeFromCodeboxResult(privateRuntimeShapeRequest, {
  success: true,
  run: {
    agentResult: {
      reply: 'Private reply must not leak into outputs.',
    },
  },
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    result: { outputs: {} },
  },
});
assert.equal(privateReplyWithPublicEnvelopeOutcome.status, 'succeeded');
assert.equal(privateReplyWithPublicEnvelopeOutcome.outputs.reply, undefined);
const replayContextOutcome = agentTaskOutcomeFromCodeboxResult({
  ...privateRuntimeShapeRequest,
  task_id: 'replay context task',
}, {
  success: false,
  status: 'failed',
  generated_recipe_path: '/tmp/generated recipe.json',
  metadata: {
    codebox_binary_path: '/opt/wp-codebox/bin/wp-codebox',
    codebox_version: '0.9.1',
    codebox_fingerprint: 'sha256:codebox',
    supported_recipe_commands: ['wordpress.load', 'wordpress.editor-validate-blocks'],
  },
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'failed',
    result: { outputs: {} },
  },
});
assert.equal(replayContextOutcome.metadata.runtime_context.schema, 'homeboy/provider-runtime-context/v1');
assert.equal(replayContextOutcome.metadata.runtime_context.provider, 'wp-codebox');
assert.equal(replayContextOutcome.metadata.runtime_context.binary_path, '/opt/wp-codebox/bin/wp-codebox');
assert.equal(replayContextOutcome.metadata.runtime_context.version, '0.9.1');
assert.equal(replayContextOutcome.metadata.runtime_context.fingerprint, 'sha256:codebox');
assert.deepEqual(replayContextOutcome.metadata.runtime_context.capabilities, ['wordpress.load', 'wordpress.editor-validate-blocks']);
assert.equal(
  replayContextOutcome.metadata.replay_command,
  "/opt/wp-codebox/bin/wp-codebox recipe-run --recipe '/tmp/generated recipe.json' --artifacts /tmp/homeboy-codebox-replay/replay-context-task --json"
);
const reconciledRunSummary = reconcileRunSummaryWithPublicEnvelope({
  status: 'failed',
  success: false,
  failure_classification: 'execution_failed',
  metadata: { terminal_status: 'incomplete' },
}, {
  success: false,
  status: 'failed',
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    success: true,
    result: {
      outputs: { reply: 'Public reply' },
    },
  },
});
assert.equal(reconciledRunSummary.status, 'succeeded');
assert.equal(reconciledRunSummary.success, true);
assert.equal(reconciledRunSummary.failure_classification, undefined);
assert.equal(reconciledRunSummary.metadata.public_envelope_success, true);
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
assert.equal(runtime.agent_task_executors[0].id, provider.id);
assert.equal(runtime.agent_task_executors[0].backend, provider.backend);
assert.equal(runtime.agent_task_executors[0].runtime_id, provider.runtime_id);
assert.equal(runtime.agent_task_executors[0].integration_contract, provider.integration_contract);
assert.equal(Object.hasOwn(runtime.agent_task_executors[0], 'deprecated_compatibility_aliases'), false);
assert.equal(runtime.agent_task_executors[0].upstream_primitive_requirements.some((requirement) => requirement.id === 'run-agent-task' && requirement.schema === 'wp-codebox/run-agent-task/v1'), true);
assert.deepEqual(provider.provider_runtime_invocation, providerRuntimeInvocationContract());
assert.deepEqual(provider.runner_readiness, [{
  id: 'wp-codebox.runner',
  label: 'WP Codebox managed runner',
  required_extensions: ['wordpress'],
  invocation: {
    schema: 'homeboy/command-invocation/v1',
    argv: ['node', '{{runtime_path}}/scripts/agent/homeboy-wp-codebox-runner-readiness.cjs'],
    display: 'node {{runtime_path}}/scripts/agent/homeboy-wp-codebox-runner-readiness.cjs',
  },
  remediation: 'homeboy extension setup wordpress',
}]);
assert.deepEqual(provider.runner_sources, [{
  id: 'wp-codebox.source',
  label: 'WP Codebox source cache',
  path: '~/.cache/homeboy/wp-codebox/source',
  remote_url: 'https://github.com/Automattic/wp-codebox.git',
  git_ref: 'main',
  remediation: 'homeboy extension setup wordpress',
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

const genericAgentTaskRequest = {
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'generic-wordpress-task-1',
  executor: {
    backend: 'wp-codebox',
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
    runtime_task: {
      ability: 'wordpress/site-health',
      input: { include_debug: false },
    },
  },
};
const taskInput = codeboxTaskRequestFromAgentTaskRequest(genericAgentTaskRequest);

assert.equal(taskInput.schema, 'wp-codebox/task-input/v1');
assert.equal(taskInput.parent_request.executor.backend, 'wp-codebox');
assert.equal(Object.hasOwn(taskInput, 'agent'), false);
assert.deepEqual(taskInput.runtime_task, {
  ability: 'wordpress/site-health',
  input: { include_debug: false },
});
assert.deepEqual(taskInput.ability_requirements, ['wordpress/site-health']);
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
const fanoutAgentTaskRequest = {
  ...genericAgentTaskRequest,
  task_id: 'generic-fanout-task-1',
  parent_plan_id: 'homeboy-plan-1',
  group_key: 'site-generation',
  executor: {
    backend: 'wp-codebox',
    model: 'gpt-5.5',
    secret_env: ['AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN'],
    config: {
      provider: 'codex',
      model: 'gpt-5.5',
      fanout_request: {
        concurrency: 2,
        workers: [
          { id: 'design', goal: 'Generate design artifact' },
          { id: 'verify', goal: 'Verify design artifact', depends_on: ['design'] },
        ],
      },
    },
  },
};
const fanoutRequest = codeboxFanoutRequestFromAgentTaskRequest(
  fanoutAgentTaskRequest,
  fanoutAgentTaskRequest.executor.config,
  fanoutAgentTaskRequest.inputs,
  {}
);
assert.equal(fanoutRequest.schema, 'wp-codebox/agent-fanout-request/v1');
assert.deepEqual(fanoutRequest.workers.map((worker) => worker.id), ['design', 'verify']);
assert.deepEqual(fanoutRequest.workers[1].dependsOn, ['design']);
assert.equal(fanoutRequest.orchestrator.agent_task_id, 'generic-fanout-task-1');
assert.equal(fanoutRequest.orchestrator.parent_plan_id, 'homeboy-plan-1');
assert.equal(fanoutRequest.orchestrator.provider, 'codex');
assert.equal(fanoutRequest.orchestrator.model, 'gpt-5.5');
assert.deepEqual(fanoutRequest.orchestrator.secret_env_names, ['AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN']);
assert.equal(fanoutRequest.metadata.homeboy_agent_task.group_key, 'site-generation');
assert.equal(codeboxTaskRequestFromAgentTaskRequest(fanoutAgentTaskRequest).fanout_request.schema, 'wp-codebox/agent-fanout-request/v1');

const controllerRuntimeTaskFanoutRequest = codeboxFanoutRequestFromAgentTaskRequest({
  ...fanoutAgentTaskRequest,
  task_id: 'controller-runtime-task-fanout-1',
  executor: {
    ...fanoutAgentTaskRequest.executor,
    config: {
      ...fanoutAgentTaskRequest.executor.config,
      fanout_request: {
        workers: [{
          id: 'concept',
          goal: 'Generate concept packet',
          runtime_task: {
            ability: 'wp-codebox/run-runtime-package',
            input: { package: { slug: 'website-idea-agent', source: 'bundles/website-idea-agent' } },
          },
        }],
      },
    },
  },
}, {
  ...fanoutAgentTaskRequest.executor.config,
  fanout_request: {
    workers: [{
      id: 'concept',
      goal: 'Generate concept packet',
      runtime_task: {
        ability: 'wp-codebox/run-runtime-package',
        input: { package: { slug: 'website-idea-agent', source: 'bundles/website-idea-agent' } },
      },
    }],
  },
}, fanoutAgentTaskRequest.inputs, {});
assert.equal(controllerRuntimeTaskFanoutRequest.workers[0].runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.equal(Object.hasOwn(controllerRuntimeTaskFanoutRequest.workers[0].runtime_task.ability_normalization, 'deprecated_compatibility_alias'), false);
assert.equal(controllerRuntimeTaskFanoutRequest.workers[0].runtime_task.input.schema, 'wp-codebox/runtime-package-task/v1');
assert.deepEqual(controllerRuntimeTaskFanoutRequest.workers[0].runtime_task.input.package, { slug: 'website-idea-agent', source: 'bundles/website-idea-agent' });
assert.deepEqual(controllerRuntimeTaskFanoutRequest.workers[0].ability_requirements, ['wp-codebox/run-runtime-package']);

const controllerAbilityRequestFanoutRequest = codeboxFanoutRequestFromAgentTaskRequest({
  ...fanoutAgentTaskRequest,
  task_id: 'controller-ability-request-fanout-1',
  executor: {
    ...fanoutAgentTaskRequest.executor,
    config: {
      ...fanoutAgentTaskRequest.executor.config,
      fanout_request: {
        workers: [{
          id: 'design',
          goal: 'Generate design packet',
          runtime_task: {
            ability: 'wp-codebox/run-runtime-package',
            input: { package: { slug: 'design-agent', source: 'bundles/design-agent' } },
          },
          ability_requirements: ['wordpress/site-health'],
        }],
      },
    },
  },
}, {
  ...fanoutAgentTaskRequest.executor.config,
  fanout_request: {
    workers: [{
      id: 'design',
      goal: 'Generate design packet',
      runtime_task: {
        ability: 'wp-codebox/run-runtime-package',
        input: { package: { slug: 'design-agent', source: 'bundles/design-agent' } },
      },
      ability_requirements: ['wordpress/site-health'],
    }],
  },
}, fanoutAgentTaskRequest.inputs, {});
assert.equal(controllerAbilityRequestFanoutRequest.workers[0].runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.equal(Object.hasOwn(controllerAbilityRequestFanoutRequest.workers[0].runtime_task.ability_normalization, 'deprecated_compatibility_alias'), false);
assert.equal(controllerAbilityRequestFanoutRequest.workers[0].runtime_task.input.schema, 'wp-codebox/runtime-package-task/v1');
assert.deepEqual(controllerAbilityRequestFanoutRequest.workers[0].runtime_task.input.package, { slug: 'design-agent', source: 'bundles/design-agent' });
assert.deepEqual(controllerAbilityRequestFanoutRequest.workers[0].ability_requirements, ['wp-codebox/run-runtime-package', 'wordpress/site-health']);
const stableCodeboxInvocation = codeboxRunAgentTaskInvocation({ taskInput });
assert.equal(stableCodeboxInvocation.contract, runtimeContractSchemas().agentTask.runRequest);
assert.equal(stableCodeboxInvocation.input.schema, runtimeContractSchemas().agentTask.runRequest);
assert.equal(stableCodeboxInvocation.args[0], 'run-agent-task');
assert.equal(stableCodeboxInvocation.result_schema, runtimeContractSchemas().agentTask.runResult);
assert.equal(stableCodeboxInvocation.implementation, 'stable-run-agent-task');
const stableCodeboxResult = {
  schema: runtimeContractSchemas().agentTask.runResult,
  status: 'succeeded',
  summary: 'Stable Codebox run succeeded.',
  artifact_result: {
    schema: runtimeContractSchemas().artifact.resultEnvelope,
    status: 'created',
    typed_artifacts: [{ name: 'stable-review', type: 'json', payload: { ok: true } }],
  },
};
const stableCodeboxOutcome = agentTaskOutcomeFromCodeboxResult(genericAgentTaskRequest, stableCodeboxResult);
assert.equal(stableCodeboxOutcome.status, 'succeeded');
assert.equal(stableCodeboxOutcome.outputs.typed_artifacts['stable-review'].payload.ok, true);
const legacyShapedCodeboxResult = {
  schema: runtimeContractSchemas().agentTask.legacyRunResponse,
  status: 'succeeded',
  metadata: {
    agent_runtime: {
      result: {
        schema: runtimeContractSchemas().artifact.resultEnvelope,
        status: 'created',
        typed_artifacts: [{ name: 'legacy-review', type: 'json', payload: { ok: true } }],
      },
    },
  },
};
assert.equal(artifactResultEnvelopeFromCodeboxResult(legacyShapedCodeboxResult), null);
assert.deepEqual(typedArtifactsFromCodeboxResult(legacyShapedCodeboxResult), {});

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
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
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
    backend: 'wp-codebox',
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
    backend: 'wp-codebox',
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
assert.equal(runtimeProfileDependencyTaskInput.runtime_requirements.component_contracts, undefined);
assert.equal(runtimeProfileDependencyTaskInput.runtime_requirements.extra_plugins, undefined);
assert.deepEqual(runtimeProfileDependencyTaskInput.component_contracts, []);
assert.equal(runtimeProfileDependencyTaskInput.runtime_requirements.upstream_primitive_requirements, undefined);

const customRuntimePolicyTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'custom-runtime-policy-task-1',
  executor: {
    backend: 'wp-codebox',
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

const previousRuntimeComponentEnv = process.env.HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT;
process.env.HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT = '/components/wp-codebox-runtime-plugin';
const envRuntimeComponentTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'env-runtime-component-task-1',
  executor: { backend: 'wp-codebox', config: { provider: 'codex' } },
  instructions: 'Run with a runtime component supplied by the selected runner environment.',
});
if (previousRuntimeComponentEnv === undefined) {
  delete process.env.HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT;
} else {
  process.env.HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT = previousRuntimeComponentEnv;
}
assert.equal(envRuntimeComponentTaskInput.runtime_component_paths.runtime, '/components/wp-codebox-runtime-plugin');

const runtimeInvocationTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'generic-provider-runtime-invocation-task-1',
  executor: {
    backend: 'wp-codebox',
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

const capabilityBundleExecutorConfig = runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
  runtime_profile: 'codebox-capability-bundle-runtime',
  runtime_profiles: {
    'codebox-capability-bundle-runtime': {
      schema: 'homeboy/runtime-profile/v1',
      id: 'codebox-capability-bundle-runtime',
      runtime_task_ability: 'example/run-task',
      capabilities: ['ability_execution'],
    },
  },
  provider: 'codex',
  ability: 'example/run-task',
  capability_bundles: ['worktree_pr_iteration'],
  runtime_invocation: { operations: { workspaceCommand: { config: { timeout_ms: 30000 } } } },
});
assert.doesNotMatch(JSON.stringify(capabilityBundleExecutorConfig), /wp-codebox\/runner-workspace-command/);
const capabilityBundleTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'capability-bundle-provider-runtime-invocation-task-1',
  executor: {
    backend: 'wp-codebox',
    config: capabilityBundleExecutorConfig,
  },
  instructions: 'Run with generic capability bundles for worktree PR iteration.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
});
assert.deepEqual(capabilityBundleTaskInput.provider_runtime_invocation.operations.workspaceCommand, runtimeInvocationTaskInput.provider_runtime_invocation.operations.workspaceCommand);
assert.deepEqual(capabilityBundleTaskInput.provider_runtime_invocation.operations.workspaceCapture, {
  task: 'wp-codebox.runner-workspace.capture',
  ability: 'wp-codebox/runner-workspace-capture',
  result_schema: 'wp-codebox/runner-workspace-capture-result/v1',
});
assert.deepEqual(capabilityBundleTaskInput.provider_runtime_invocation.operations.workspacePublish, {
  task: 'wp-codebox.runner-workspace.publish',
  ability: 'wp-codebox/runner-workspace-publish',
  result_schema: 'wp-codebox/runner-workspace-publication-result/v1',
});
assert.equal(capabilityBundleTaskInput.provider_runtime_invocation.operations.artifactHandoff.ability, 'wp-codebox/handoff-artifacts');
assert.equal(capabilityBundleTaskInput.provider_runtime_invocation.operations.toolCallTranscriptRecord.ability, 'wp-codebox/record-tool-call-transcript');

const customRuntimeProfileTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'custom-runtime-profile-task-1',
  executor: {
    backend: 'wp-codebox',
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
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  instructions: 'Run a repo-loop bundle workflow.',
  artifact_declarations: [{
    name: 'concept_packet',
    type: 'ConceptPacket',
    artifact_schema: 'example/concept-packet/v1',
    required: true,
  }],
  inputs: {
    runtime_task: {
      ability: 'example/run-agent-bundle',
      input: {
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
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  instructions: 'Run a controller workflow with domain artifact declarations in client context.',
  inputs: {
    runtime_task: {
      ability: 'wp-codebox/run-runtime-package',
      input: {
        package: { slug: 'website-idea-agent', source: 'bundles/website-idea-agent' },
        artifact_declarations: [{
          schema: 'wp-codebox/artifact-declaration/v1',
          name: 'concept_packet',
          artifact_schema: 'wp-site-generator/ConceptPacket/v1',
          required: true,
        }],
      },
    },
  },
});

assert.deepEqual(controllerClientContextArtifactsTaskInput.artifact_declarations, []);
assert.equal(controllerClientContextArtifactsTaskInput.runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.equal(controllerClientContextArtifactsTaskInput.runtime_task.input.schema, 'wp-codebox/runtime-package-task/v1');
assert.deepEqual(controllerClientContextArtifactsTaskInput.runtime_task.input.package, { slug: 'website-idea-agent', source: 'bundles/website-idea-agent' });
assert.deepEqual(controllerClientContextArtifactsTaskInput.runtime_task.input.artifact_declarations, [{
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'concept_packet',
  artifact_schema: 'wp-site-generator/ConceptPacket/v1',
  required: true,
}]);
assert.deepEqual(controllerClientContextArtifactsTaskInput.runtime_task.input.required_artifacts, ['concept_packet']);
assert.equal(Object.hasOwn(controllerClientContextArtifactsTaskInput.runtime_task.input, 'runtime_package'), false);
assert.equal(Object.hasOwn(controllerClientContextArtifactsTaskInput.runtime_task.input, 'agent'), false);
assert.equal(Object.hasOwn(controllerClientContextArtifactsTaskInput.runtime_task.input, 'metadata'), false);
assert.equal(Object.hasOwn(controllerClientContextArtifactsTaskInput.runtime_task.input, 'engine_data_outputs'), false);
assert.equal(Object.hasOwn(controllerClientContextArtifactsTaskInput.runtime_task.input, 'artifacts'), false);

const controllerClientContextRuntimeTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'controller-client-context-runtime-task-1',
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  instructions: 'Run a controller workflow with hydrated runtime task input in client context.',
  inputs: {
    runtime_task: {
      ability: 'wp-codebox/run-runtime-package',
      input: {
        package: { slug: 'static-site-agent', source: 'bundles/static-site-agent' },
        input: {
          concept_packet: { payload: { title: 'Concept' } },
          design_packet: { payload: { design_system: 'Editorial' } },
        },
      },
    },
  },
});
assert.equal(controllerClientContextRuntimeTaskInput.runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.equal(controllerClientContextRuntimeTaskInput.runtime_task.input.schema, 'wp-codebox/runtime-package-task/v1');
assert.equal(controllerClientContextRuntimeTaskInput.runtime_task.input.input.concept_packet.payload.title, 'Concept');
assert.equal(controllerClientContextRuntimeTaskInput.runtime_task.input.input.design_packet.payload.design_system, 'Editorial');

const canonicalRuntimePackageTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'canonical-runtime-package-task-1',
  executor: { backend: 'wp-codebox', model: 'gpt-5.5', config: { provider: 'codex' } },
  instructions: 'Run a runtime-package task through the public Codebox ability.',
  inputs: {
    runtime_task: {
      ability: 'wp-codebox/run-runtime-package',
      input: { package: { slug: 'example-agent', source: 'bundles/example-agent' } },
    },
  },
});
assert.equal(canonicalRuntimePackageTaskInput.runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.equal(Object.hasOwn(canonicalRuntimePackageTaskInput.runtime_task.ability_normalization, 'deprecated_compatibility_alias'), false);
assert.equal(canonicalRuntimePackageTaskInput.runtime_task.input.schema, 'wp-codebox/runtime-package-task/v1');
assert.deepEqual(canonicalRuntimePackageTaskInput.runtime_task.input.package, { slug: 'example-agent', source: 'bundles/example-agent' });

const neutralProfileRuntimePackageTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'neutral-profile-runtime-package-task-1',
  executor: {
    backend: 'wp-codebox',
    model: 'gpt-5.5',
    config: {
      provider: 'codex',
      runtime_profile: 'example-agent-profile',
      runtime_profiles: {
        'example-agent-profile': {
          schema: 'homeboy/runtime-profile/v1',
          id: 'example-agent-profile',
          runtime_task_ability: 'wp-codebox/run-runtime-package',
        },
      },
      runtime_task: { ability: 'wp-codebox/run-runtime-package', input: {} },
    },
  },
  instructions: 'Run a neutral runtime-package task through the selected runtime profile.',
});
assert.equal(neutralProfileRuntimePackageTaskInput.runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.equal(neutralProfileRuntimePackageTaskInput.runtime_task.input.schema, 'wp-codebox/runtime-package-task/v1');
assert.deepEqual(neutralProfileRuntimePackageTaskInput.runtime_task.input.package, { slug: 'example-agent-profile' });
assert.equal(neutralProfileRuntimePackageTaskInput.runtime_task.input.provider, 'codex');
assert.equal(neutralProfileRuntimePackageTaskInput.runtime_task.input.model, 'gpt-5.5');

const explicitRuntimePackageComponentTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'runtime-package-substrate-task-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'codex',
      component_contracts: [
        { slug: 'agents-api', path: '/components/agents-api' },
        { slug: 'sample-plugin', path: '/components/sample-plugin' },
        { slug: 'sample-plugin-tools', path: '/components/sample-plugin-tools' },
      ],
    },
  },
  instructions: 'Run a runtime-package task with caller-supplied component contracts.',
  inputs: {
    runtime_task: {
      ability: 'wp-codebox/run-runtime-package',
      input: { package: { slug: 'example-agent' } },
    },
  },
}, {
  componentPathDefaults: {
    contract_slug_map: {
      'agents-api': 'agents_api',
      'sample-plugin': 'agent_runtime',
      'sample-plugin-tools': 'agent_runtime_tools',
    },
    path_aliases: {
      agent_runtime: ['contract:agent_runtime'],
      agent_runtime_tools: ['contract:agent_runtime_tools'],
    },
  },
});
assert.equal(explicitRuntimePackageComponentTaskInput.runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.deepEqual(explicitRuntimePackageComponentTaskInput.component_contracts.map((contract) => contract.slug), ['agents-api', 'sample-plugin', 'sample-plugin-tools']);
assert.equal(explicitRuntimePackageComponentTaskInput.runtime_component_paths.agents_api, '/components/agents-api');
assert.equal(explicitRuntimePackageComponentTaskInput.runtime_component_paths.agent_runtime, '/components/sample-plugin');
assert.equal(explicitRuntimePackageComponentTaskInput.runtime_component_paths.agent_runtime_tools, '/components/sample-plugin-tools');
assert.equal(explicitRuntimePackageComponentTaskInput.runtime_env.WP_CODEBOX_AGENTS_API_PATH, '/components/agents-api');
assert.equal(explicitRuntimePackageComponentTaskInput.runtime_env.WP_CODEBOX_DATA_MACHINE_PATH, undefined);
assert.equal(explicitRuntimePackageComponentTaskInput.runtime_env.WP_CODEBOX_DATA_MACHINE_CODE_PATH, undefined);

const previousStaleDataMachinePath = process.env.WP_CODEBOX_DATA_MACHINE_PATH;
process.env.WP_CODEBOX_DATA_MACHINE_PATH = '/components/stale-data-machine';
const explicitRuntimeComponentOverridesEnvTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'runtime-package-explicit-component-overrides-env-task-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'codex',
      runtime_component_paths: {
        agent_runtime: '/components/explicit-sample-plugin',
      },
    },
  },
  instructions: 'Run a runtime-package task with an explicit sample component path.',
  inputs: {
    ability_request: {
      name: 'runtime-package/run',
      input: { package: { slug: 'example-agent' } },
    },
  },
});
restoreEnv('WP_CODEBOX_DATA_MACHINE_PATH', previousStaleDataMachinePath);
assert.equal(explicitRuntimeComponentOverridesEnvTaskInput.runtime_component_paths.agent_runtime, '/components/explicit-sample-plugin');
assert.equal(explicitRuntimeComponentOverridesEnvTaskInput.runtime_env.WP_CODEBOX_DATA_MACHINE_PATH, undefined);

const runtimePackageWithoutSubstrateTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'runtime-package-missing-substrate-task-1',
  executor: { backend: 'wp-codebox', config: { provider: 'codex' } },
  instructions: 'Run a runtime-package task through the public Codebox ability without HBE substrate construction.',
  inputs: {
    runtime_task: {
      ability: 'wp-codebox/run-runtime-package',
      input: { package: { slug: 'example-agent' } },
    },
  },
});
assert.equal(runtimePackageWithoutSubstrateTaskInput.runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.deepEqual(runtimePackageWithoutSubstrateTaskInput.component_contracts, []);

const previousAgentsApiPath = process.env.WP_CODEBOX_AGENTS_API_PATH;
const previousDataMachinePath = process.env.WP_CODEBOX_DATA_MACHINE_PATH;
const previousDataMachineCodePath = process.env.WP_CODEBOX_DATA_MACHINE_CODE_PATH;
process.env.WP_CODEBOX_AGENTS_API_PATH = workspaceRoot;
process.env.WP_CODEBOX_DATA_MACHINE_PATH = workspaceRoot;
process.env.WP_CODEBOX_DATA_MACHINE_CODE_PATH = workspaceRoot;
const runtimePackageEnvSubstrateTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'runtime-package-env-substrate-task-1',
  executor: { backend: 'wp-codebox', config: { provider: 'codex' } },
  instructions: 'Run a runtime-package task with env-declared Codebox-owned substrate components.',
  inputs: {
    runtime_task: {
      ability: 'wp-codebox/run-runtime-package',
      input: { package: { slug: 'example-agent' } },
    },
  },
});
restoreEnv('WP_CODEBOX_AGENTS_API_PATH', previousAgentsApiPath);
restoreEnv('WP_CODEBOX_DATA_MACHINE_PATH', previousDataMachinePath);
restoreEnv('WP_CODEBOX_DATA_MACHINE_CODE_PATH', previousDataMachineCodePath);
assert.deepEqual(runtimePackageEnvSubstrateTaskInput.component_contracts, []);
assert.equal(runtimePackageEnvSubstrateTaskInput.runtime_component_paths.agents_api, workspaceRoot);
assert.equal(runtimePackageEnvSubstrateTaskInput.runtime_component_paths.agent_runtime, undefined);
assert.equal(runtimePackageEnvSubstrateTaskInput.runtime_component_paths.data_machine_code, undefined);

const explicitRuntimeTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'explicit-runtime-task-1',
  executor: { backend: 'wp-codebox', model: 'gpt-5.5', config: { provider: 'codex' } },
  instructions: 'Run an explicit runtime task through the public Codebox ability.',
  inputs: {
    runtime_task: {
      kind: 'bundle',
      ability: 'wp-codebox/run-runtime-package',
      input: { package: { slug: 'example-agent' } },
    },
  },
});
assert.equal(explicitRuntimeTaskInput.runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.equal(Object.hasOwn(explicitRuntimeTaskInput.runtime_task.ability_normalization, 'deprecated_compatibility_alias'), false);
assert.equal(explicitRuntimeTaskInput.runtime_task.input.schema, 'wp-codebox/runtime-package-task/v1');
assert.deepEqual(explicitRuntimeTaskInput.runtime_task.input.package, { slug: 'example-agent' });
assert.equal(explicitRuntimeTaskInput.runtime_task.input.provider, 'codex');
assert.equal(explicitRuntimeTaskInput.runtime_task.input.model, 'gpt-5.5');
assert.equal(Object.hasOwn(explicitRuntimeTaskInput.runtime_task.input, 'runtime_package'), false);
assert.equal(Object.hasOwn(explicitRuntimeTaskInput.runtime_task.input, 'agent'), false);
assert.equal(Object.hasOwn(explicitRuntimeTaskInput.runtime_task.input, 'metadata'), false);

const runtimeConfigOptionsRuntimeTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'runtime-config-options-runtime-task-1',
  executor: { backend: 'wp-codebox', config: {} },
  instructions: 'Run an explicit runtime task with controller runtime-config options.',
  inputs: {
    runtime_task: {
      kind: 'bundle',
      ability: 'wp-codebox/run-runtime-package',
      input: {
        package: { slug: 'example-agent' },
        options: { provider: 'codex', model: 'gpt-5.5' },
      },
    },
  },
});
assert.equal(runtimeConfigOptionsRuntimeTaskInput.runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.equal(runtimeConfigOptionsRuntimeTaskInput.runtime_task.input.provider, 'codex');
assert.equal(runtimeConfigOptionsRuntimeTaskInput.runtime_task.input.model, 'gpt-5.5');
assert.deepEqual(runtimeConfigOptionsRuntimeTaskInput.runtime_task.input.options, { provider: 'codex', model: 'gpt-5.5' });

const providerAndControllerArtifactsTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'provider-and-controller-artifacts-task-1',
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  instructions: 'Run a controller workflow with provider and domain artifact declarations.',
  artifact_declarations: [{
    name: 'patch',
    required: true,
  }],
  inputs: {
    runtime_task: {
      ability: 'wp-codebox/run-runtime-package',
      input: {
        artifact_declarations: [{
          schema: 'wp-codebox/artifact-declaration/v1',
          name: 'concept_packet',
          artifact_schema: 'wp-site-generator/ConceptPacket/v1',
          required: true,
        }],
      },
    },
  },
});
assert.deepEqual(providerAndControllerArtifactsTaskInput.artifact_declarations.map((declaration) => declaration.name), ['patch']);
assert.equal(providerAndControllerArtifactsTaskInput.runtime_task.input.schema, 'wp-codebox/runtime-package-task/v1');
assert.deepEqual(providerAndControllerArtifactsTaskInput.runtime_task.input.artifact_declarations, [{
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'concept_packet',
  artifact_schema: 'wp-site-generator/ConceptPacket/v1',
  required: true,
}, {
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'patch',
  required: true,
}]);
assert.deepEqual(providerAndControllerArtifactsTaskInput.runtime_task.input.required_artifacts, ['concept_packet', 'patch']);

const placeholderArtifactOutcome = agentTaskOutcomeFromCodeboxResult({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'placeholder-artifact-task-1',
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  artifact_declarations: [
    { name: 'patch', required: true },
    { name: 'agent_result', required: true },
    {
      name: 'concept_packet',
      artifact_schema: 'wp-site-generator/ConceptPacket/v1',
      required: true,
    },
  ],
}, {
  success: true,
  status: 'completed',
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    result: {
      outputs: {
        reply: '<workspace_ls path="/workspace/wp-site-generator" />',
      },
    },
  },
});
assert.equal(placeholderArtifactOutcome.status, 'failed');
assert.equal(placeholderArtifactOutcome.failure_classification, 'execution_failed');
assert.deepEqual(
  placeholderArtifactOutcome.diagnostics.map((diagnostic) => diagnostic.class),
  ['codebox.required_typed_artifacts_missing']
);
assert.match(placeholderArtifactOutcome.summary, /did not produce required typed artifacts: concept_packet/);

const eventNormalizerRequest = {
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'codebox-events-task-1',
  group_key: 'codebox-events-group',
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  instructions: 'Normalize Codebox event artifacts.',
  inputs: {},
};
const eventNormalizerArtifactRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-events-'));
const eventFile = path.join(eventNormalizerArtifactRoot, 'events.json');
const missingEventFile = path.join(eventNormalizerArtifactRoot, 'missing-events.json');
fs.writeFileSync(eventFile, `${JSON.stringify({
  events: [
    {
      schema: 'wp-codebox/fanout-worker-event/v1',
      id: 'worker-b-started',
      name: 'worker.started',
      worker_id: 'worker-b',
      sequence: 1,
      created_at: '2026-06-25T10:00:02.000Z',
    },
    {
      schema: 'wp-codebox/fanout-worker-event/v1',
      id: 'worker-a-started',
      name: 'worker.started',
      worker_id: 'worker-a',
      sequence: 1,
      created_at: '2026-06-25T10:00:01.000Z',
      artifact_refs: [{ kind: 'codebox-worker-log', path: 'artifacts/worker-a.log' }],
    },
  ],
}, null, 2)}\n`);
const normalizedCodeboxEvents = normalizeCodeboxAgentTaskEvents(eventNormalizerRequest, {
  events_file: eventFile,
  result_path: missingEventFile,
  stdout: JSON.stringify([{
    schema: 'wp-codebox/fanout-worker-event/v1',
    id: 'worker-a-failed',
    name: 'worker.failed',
    status: 'failed',
    worker_id: 'worker-a',
    sequence: 2,
    created_at: '2026-06-25T10:00:03.000Z',
    diagnostics: [{ class: 'worker.timeout', message: 'Worker timed out.', data: { timeout_ms: 30000 } }],
  }]),
});
assert.deepEqual(normalizedCodeboxEvents.events.map((event) => event.event_id), [
  'worker-a-started',
  'worker-b-started',
  'worker-a-failed',
]);
assert.deepEqual(normalizedCodeboxEvents.events.map((event) => event.sequence), [1, 2, 3]);
assert.equal(normalizedCodeboxEvents.events[0].schema, 'homeboy/agent-task-event/v1');
assert.equal(normalizedCodeboxEvents.events[0].task_id, eventNormalizerRequest.task_id);
assert.equal(normalizedCodeboxEvents.events[0].worker_id, 'worker-a');
assert.equal(normalizedCodeboxEvents.events[0].artifacts[0].path, 'artifacts/worker-a.log');
assert.equal(normalizedCodeboxEvents.events[2].status, 'failed');
assert.equal(normalizedCodeboxEvents.events[2].diagnostics[0].class, 'worker.timeout');
assert.equal(normalizedCodeboxEvents.diagnostics[0].class, 'codebox.events_file_missing');
const eventOutcome = agentTaskOutcomeFromCodeboxResult(eventNormalizerRequest, {
  success: false,
  status: 'timeout',
  summary: 'One fanout worker timed out.',
  events_file: eventFile,
  result_path: missingEventFile,
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    artifact_refs: [{ kind: 'codebox-result', path: 'artifacts/result.json' }],
    result: { outputs: {} },
  },
});
assert.equal(eventOutcome.status, 'timeout');
assert.equal(eventOutcome.events.length, 2);
assert.equal(eventOutcome.events[0].artifacts[0].path, 'artifacts/worker-a.log');
assert.equal(eventOutcome.artifacts.some((artifact) => artifact.path === 'artifacts/result.json'), true);
assert.equal(eventOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.events_file_missing'), true);
assert.equal(eventOutcome.metadata.normalized_events.length, 2);

const missingTypedArtifactOutcome = agentTaskOutcomeFromCodeboxResult({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'missing-typed-artifact-task-1',
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  artifact_declarations: [{
    name: 'concept_packet',
    artifact_schema: 'wp-site-generator/ConceptPacket/v1',
    required: true,
  }],
}, {
  success: true,
  status: 'completed',
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    result: { outputs: {} },
  },
});
assert.equal(missingTypedArtifactOutcome.status, 'failed');
assert.equal(missingTypedArtifactOutcome.failure_classification, 'execution_failed');
assert.deepEqual(
  missingTypedArtifactOutcome.diagnostics.map((diagnostic) => diagnostic.class),
  ['codebox.required_typed_artifacts_missing']
);

const genericRepoLoopTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'repo-loop-generic-ability-task-1',
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  instructions: 'Run a generic declared ability with workflow inputs.',
  client_context: {
    inputs: {
      packet: 'artifact-42',
      dry_run: true,
    },
  },
  inputs: {
    runtime_task: {
      ability: 'example/materialize-artifact',
      input: { packet: 'artifact-42', format: 'json', dry_run: false },
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
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  instructions: 'Run a generic declared ability without ambient client context merge.',
  client_context: {
    inputs: {
      packet: 'ambient-artifact',
      dry_run: true,
    },
  },
  inputs: {
    runtime_task: {
      ability: 'example/materialize-artifact',
      input: { explicit: true },
    },
  },
});
assert.deepEqual(noImplicitClientContextTaskInput.runtime_task.input, { explicit: true });

const repoLoopWorkspaceBase = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wordpress-repo-loop-workspace-'));
const repoLoopWorkspaceRoot = path.join(repoLoopWorkspaceBase, 'wp-site-generator@wpsg-lab-proof-20260622-2102');
fs.mkdirSync(repoLoopWorkspaceRoot, { recursive: true });
const repoLoopWorkspaceTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'repo-loop-workspace-task-1',
  cwd: repoLoopWorkspaceRoot,
  repo: 'example-repo@example-loop-main-20260616',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'openai',
      workspace_required: true,
    },
  },
  instructions: 'Run a repo-loop workflow against the current checkout.',
  inputs: {
    runtime_task: { ability: 'example/run-agent-bundle', input: {} },
  },
});

const repoLoopWorkspaceMount = repoLoopWorkspaceTaskInput.mounts.find(
  (mount) => mount.metadata?.kind === 'homeboy-runtime-workspace'
);
assert(repoLoopWorkspaceMount, 'repo-loop cwd is translated into a Codebox workspace mount');
assert.equal(repoLoopWorkspaceMount.source, repoLoopWorkspaceRoot);
assert.equal(repoLoopWorkspaceMount.target, '/workspace/wp-site-generator');
assert.equal(repoLoopWorkspaceMount.mode, 'readwrite');
assert.deepEqual(repoLoopWorkspaceMount.metadata, {
  kind: 'homeboy-runtime-workspace',
  workspace_slug: 'wp-site-generator',
  workspaceRef: 'wp-site-generator@wpsg-lab-proof-20260622-2102',
  artifactExcludePaths: ['.ci/**'],
});
// The runner materializes its dependency checkouts into `.ci/` inside the target
// repo working tree; the workspace mount must exclude that directory from the
// captured sandbox patch so dependency clones do not masquerade as agent changes.
assert.deepEqual(repoLoopWorkspaceMount.metadata.artifactExcludePaths, ['.ci/**']);
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

const genericRuntimeAliasTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'generic-runtime-alias-task-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'openai',
      runtime_components: {
        example_runtime: '/tmp/example-runtime',
      },
      component_path_aliases: {
        agent_runtime: ['runtime_component:example_runtime'],
      },
    },
  },
  instructions: 'Accept explicit runtime component aliases.',
  inputs: {
    runtime_task: { ability: 'example/run-agent-bundle', input: {} },
  },
});

assert.equal(genericRuntimeAliasTaskInput.runtime_component_paths.agent_runtime, '/tmp/example-runtime');
assert.deepEqual(genericRuntimeAliasTaskInput.compatibility_diagnostics, undefined);

const repoLoopTypedOutputsTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'repo-loop-typed-outputs-task-1',
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  instructions: 'Run a repo-loop step that declares typed outputs generically.',
  outputs: {
    typed_artifacts: [{
      name: 'concept_packet',
      type: 'ConceptPacket',
      artifact_schema: 'example/concept-packet/v1',
    }],
  },
  inputs: {
    runtime_task: { ability: 'example/run-agent-bundle', input: {} },
  },
});

assert.deepEqual(repoLoopTypedOutputsTaskInput.artifact_declarations, []);

const legacyTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'legacy-codebox-task-1',
  executor: { backend: 'wp-codebox', config: { provider: 'openai' } },
  instructions: 'Already queued legacy request.',
  inputs: {},
});

assert.equal(legacyTaskInput.schema, 'wp-codebox/task-input/v1');

const explicitAgentTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'explicit-agent-codebox-task-1',
  executor: {
    backend: 'wp-codebox',
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
    executor: { backend: 'wp-codebox', config: { provider: 'codex' } },
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
const plannedSecretEnvPlan = {
  schema: 'homeboy/secret-env-plan/v1',
  secret_env_names: ['HOMEBOY_PLANNED_CODEBOX_SECRET'],
  env_name_mapping: {
    'wordpress.codebox-agent-task-executor': ['HOMEBOY_PLANNED_CODEBOX_SECRET'],
  },
  status: [{ name: 'HOMEBOY_PLANNED_CODEBOX_SECRET', configured: true, source: 'env' }],
};
validateSecretEnvPlan(plannedSecretEnvPlan);
process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON = JSON.stringify(plannedSecretEnvPlan);
let plannedSecretEnvTaskInput;
try {
  plannedSecretEnvTaskInput = codeboxTaskRequestFromAgentTaskRequest({
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'planned-secret-env-codebox-task-1',
    executor: {
      backend: 'wp-codebox',
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

// Regression: the consumer must honor the FULL authoritative aggregation that
// core's SecretEnvPlan::secret_env_names() folds (secret_env_plan.rs:214-235) —
// provider-credential names and requirement-derived names were previously
// dropped, so those declared secrets silently never reached the sandbox.
const previousFullPlan = process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON;
const fullSecretEnvPlan = {
  schema: 'homeboy/secret-env-plan/v1',
  secret_env_names: ['HOMEBOY_PLANNED_CODEBOX_SECRET'],
  requirements: [
    { name: 'HOMEBOY_REQUIRED_CODEBOX_SECRET', required: true },
    { name: 'HOMEBOY_OPTIONAL_CODEBOX_SECRET', required: false },
  ],
  provider_credentials: {
    codex: {
      secret_env: ['AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN', 'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN'],
    },
  },
  env_name_mapping: {
    'wordpress.codebox-agent-task-executor': ['HOMEBOY_MAPPED_CODEBOX_SECRET'],
  },
  status: [{ name: 'HOMEBOY_PLANNED_CODEBOX_SECRET', configured: true, source: 'env' }],
};
validateSecretEnvPlan(fullSecretEnvPlan);
process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON = JSON.stringify(fullSecretEnvPlan);
let fullPlanSecretEnvTaskInput;
try {
  fullPlanSecretEnvTaskInput = codeboxTaskRequestFromAgentTaskRequest({
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'full-plan-secret-env-codebox-task-1',
    executor: {
      backend: 'wp-codebox',
      config: { provider: 'codex' },
    },
    instructions: 'Run a Codex-backed Codebox task with a fully aggregated secret env plan.',
    inputs: {},
  });
} finally {
  if (previousFullPlan === undefined) {
    delete process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON;
  } else {
    process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON = previousFullPlan;
  }
}
for (const declaredSecret of [
  'HOMEBOY_PLANNED_CODEBOX_SECRET',
  'HOMEBOY_REQUIRED_CODEBOX_SECRET',
  'HOMEBOY_OPTIONAL_CODEBOX_SECRET',
  'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  'HOMEBOY_MAPPED_CODEBOX_SECRET',
]) {
  assert.equal(
    fullPlanSecretEnvTaskInput.secret_env.includes(declaredSecret),
    true,
    `expected forwarded secret_env to include ${declaredSecret}`,
  );
}

const claudeCodeTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'claude-code-codebox-task-1',
  executor: {
    backend: 'wp-codebox',
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
    executor: { backend: 'wp-codebox', config: { provider: 'codex' } },
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

const explicitProviderPath = path.join(providerRoot, 'explicit-provider-plugin');
fs.mkdirSync(explicitProviderPath, { recursive: true });
process.env.HOMEBOY_SETTINGS_JSON = JSON.stringify({
  provider_plugin_paths: { codex: ['/missing/stale-openai-provider'] },
});
let explicitCodexTaskInput;
try {
  explicitCodexTaskInput = codeboxTaskRequestFromAgentTaskRequest({
    schema: 'homeboy/agent-task-request/v1',
    task_id: 'explicit-codex-provider-path-codebox-task-1',
    executor: {
      backend: 'wp-codebox',
      config: {
        provider: 'codex',
        runtime_options: { providerPluginPaths: [explicitProviderPath] },
      },
    },
    instructions: 'Run a Codex-backed Codebox task with an explicit provider checkout.',
    inputs: {},
  });
} finally {
  if (previousHomeboySettingsJson === undefined) {
    delete process.env.HOMEBOY_SETTINGS_JSON;
  } else {
    process.env.HOMEBOY_SETTINGS_JSON = previousHomeboySettingsJson;
  }
}
assert.deepEqual(explicitCodexTaskInput.provider_plugin_paths, [explicitProviderPath]);
assert.equal(explicitCodexTaskInput.runtime_requirements.provider_plugins[0].path, explicitProviderPath);
assert.equal(JSON.stringify(explicitCodexTaskInput.runtime_requirements.provider_plugins).includes('/missing/stale-openai-provider'), false);

const explicitProviderPathFromConfig = path.join(providerRoot, 'explicit-provider-plugin-from-config');
fs.mkdirSync(explicitProviderPathFromConfig, { recursive: true });
const staleProviderPathFromRuntimeRequirements = '/missing/stale-runtime-requirements-provider';
const explicitCodexTaskInputFromConfig = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'explicit-codex-provider-path-config-codebox-task-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'codex',
      provider_plugin_paths: [explicitProviderPathFromConfig],
      runtime_requirements: {
        provider_plugins: [{ path: staleProviderPathFromRuntimeRequirements }],
      },
    },
  },
  instructions: 'Run a Codex-backed Codebox task with an explicit provider checkout from request config.',
  inputs: {},
});
assert.deepEqual(explicitCodexTaskInputFromConfig.provider_plugin_paths, [explicitProviderPathFromConfig]);
assert.deepEqual(explicitCodexTaskInputFromConfig.runtime_requirements.provider_plugins, [{ path: explicitProviderPathFromConfig }]);
assert.equal(JSON.stringify(explicitCodexTaskInputFromConfig.runtime_requirements).includes(staleProviderPathFromRuntimeRequirements), false);

const explicitProviderPathFromRuntimeOptions = path.join(providerRoot, 'explicit-provider-plugin-from-runtime-options');
fs.mkdirSync(explicitProviderPathFromRuntimeOptions, { recursive: true });
const staleProviderPathFromRequestConfig = '/missing/stale-request-config-provider';
const explicitCodexTaskInputFromRuntimeOptions = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'explicit-codex-provider-path-runtime-options-codebox-task-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'codex',
      provider_plugin_paths: [staleProviderPathFromRequestConfig],
    },
  },
  instructions: 'Run a Codex-backed Codebox task with an explicit provider checkout from runtime options.',
  inputs: {},
}, { providerPluginPaths: [explicitProviderPathFromRuntimeOptions] });
assert.deepEqual(explicitCodexTaskInputFromRuntimeOptions.provider_plugin_paths, [explicitProviderPathFromRuntimeOptions]);
assert.deepEqual(explicitCodexTaskInputFromRuntimeOptions.runtime_requirements.provider_plugins, [{ path: explicitProviderPathFromRuntimeOptions }]);
assert.equal(JSON.stringify(explicitCodexTaskInputFromRuntimeOptions.runtime_requirements).includes(staleProviderPathFromRequestConfig), false);

const runtimePackageWorkspaceRoot = path.join(workspaceRoot, 'example-runtime@proof-loop');
fs.mkdirSync(runtimePackageWorkspaceRoot, { recursive: true });
const runtimePackageTypedArtifactRequest = {
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'runtime-package-typed-artifact-codebox-task-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'codex',
      runtime_task: {
        ability: 'wp-codebox/run-runtime-package',
        input: {
          package: { slug: 'store-idea-agent', source: 'bundles/store-idea-agent' },
          workflow: { id: 'store-idea-artifact-flow' },
        },
      },
    },
  },
  instructions: 'Produce a typed concept packet.',
  artifact_declarations: [{
    name: 'concept_packet',
    artifact_schema: 'example/ConceptPacket/v1',
    required: true,
  }],
  inputs: {
    target: { root: runtimePackageWorkspaceRoot },
    agent_bundles: [{
      source: 'bundles/store-idea-agent',
      bundle: {
        bundle_schema_version: 1,
        bundle_slug: 'store-idea-agent',
        agent: { agent_slug: 'store-idea-agent' },
      },
    }],
  },
};
const runtimePackageTypedArtifactTaskInput = codeboxTaskRequestFromAgentTaskRequest(runtimePackageTypedArtifactRequest);
assert.equal(runtimePackageTypedArtifactTaskInput.runtime_task.ability, 'wp-codebox/run-runtime-package');
assert.equal(runtimePackageTypedArtifactTaskInput.runtime_task.input.schema, 'wp-codebox/runtime-package-task/v1');
assert.equal(runtimePackageTypedArtifactTaskInput.runtime_task.input.package.slug, 'store-idea-agent');
assert.equal(runtimePackageTypedArtifactTaskInput.runtime_task.input.package.source, '/workspace/example-runtime/bundles/store-idea-agent');
assert.equal(runtimePackageTypedArtifactTaskInput.runtime_task.input.package.bundle.bundle_slug, 'store-idea-agent');
assert.deepEqual(runtimePackageTypedArtifactTaskInput.runtime_task.input.workflow, { id: 'store-idea-artifact-flow' });
assert.equal(runtimePackageTypedArtifactTaskInput.runtime_task.input.package.source === 'bundles/store-idea-agent', false);
assert.equal(JSON.stringify(runtimePackageTypedArtifactTaskInput.runtime_task).includes('"source":"bundles/store-idea-agent"'), false);
assert.deepEqual(runtimePackageTypedArtifactTaskInput.runtime_task.input.artifact_declarations, [{
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'concept_packet',
  artifact_schema: 'example/ConceptPacket/v1',
  required: true,
}]);
assert.deepEqual(runtimePackageTypedArtifactTaskInput.runtime_task.input.required_artifacts, ['concept_packet']);
assert.equal(Object.hasOwn(runtimePackageTypedArtifactTaskInput.runtime_task.input, 'runtime_package'), false);
assert.equal(Object.hasOwn(runtimePackageTypedArtifactTaskInput.runtime_task.input, 'agent'), false);
assert.equal(Object.hasOwn(runtimePackageTypedArtifactTaskInput.runtime_task.input, 'metadata'), false);
assert.equal(Object.hasOwn(runtimePackageTypedArtifactTaskInput.runtime_task.input, 'engine_data_outputs'), false);
assert.equal(Object.hasOwn(runtimePackageTypedArtifactTaskInput.runtime_task.input, 'artifacts'), false);

const runtimePackageTypedArtifactOutcome = agentTaskOutcomeFromCodeboxResult(runtimePackageTypedArtifactRequest, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  task_input: runtimePackageTypedArtifactTaskInput,
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    result: {
      outputs: {
        typed_artifacts: {
          concept_packet: {
            payload: { title: 'Fixture concept' },
          },
        },
      },
    },
  },
});
assert.equal(runtimePackageTypedArtifactOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.required_typed_artifacts_missing'), false);
assert.deepEqual(runtimePackageTypedArtifactOutcome.outputs.typed_artifacts.concept_packet.payload, { title: 'Fixture concept' });

console.log('Codebox agent-task executor boundary contract passed');
