#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const runtimeAgentCi = require(path.join(repoRoot, 'runtime-agent-ci/provider-adapters.js'));
const {
  genericAgentTaskPlan,
  genericAgentTaskRequest,
} = require(path.join(repoRoot, 'agent-task-contracts'));

const genericBoundaryTerms = /Data Machine|DataMachine|datamachine|data-machine|wp-site-generator|WPSG|site-generator|site generator/;
const genericFiles = [
  'runtime-agent-ci/provider-adapters.js',
  'runtime-agent-ci/lib/runtime-agent-ci-plan.js',
  'agent-task-contracts/generic-agent-task-plan.js',
  '.github/workflows/runtime-agent-ci.yml',
];

for (const relativePath of genericFiles) {
  const content = fs.readFileSync(path.join(repoRoot, relativePath), 'utf8');
  assert.equal(
    genericBoundaryTerms.test(content),
    false,
    `${relativePath} must remain provider-neutral.`
  );
}

const runtimeProfile = {
  schema: 'homeboy/runtime-profile/v1',
  id: 'example-agent-ci',
  runtime_task_ability: 'example/run-task',
  runtime_bundle_ability: 'runtime/run-agent-bundle',
  runtime_workflow_ability: 'runtime/execute-workflow',
  capabilities: ['ability_execution', 'agent_bundle_execution', 'workflow_execution'],
  runtime_execution_contracts: {
    ability: {
      ability_field: 'runtime_task_ability',
      required_capabilities: ['ability_execution'],
    },
    bundle: {
      ability_field: 'runtime_bundle_ability',
      required_capabilities: ['agent_bundle_execution'],
    },
    workflow: {
      ability_field: 'runtime_workflow_ability',
      required_capabilities: ['workflow_execution'],
    },
  },
  component_path_defaults: {
    contract_slug_map: { 'example-runtime': 'agent_runtime' },
    path_aliases: { agent_runtime: ['contract:agent_runtime'] },
  },
  ability_requirements: ['example/run-task', 'example/read-state'],
};

const genericConfig = runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
  runtime: 'wp-codebox',
  provider: 'example-provider',
  model: 'example-model',
  runtimeProfile: runtimeProfile.id,
  runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
  runtimeComponentPaths: { agent_runtime: '/workspace/components/example-runtime' },
  ignoredWorkspacePaths: ['.cache', 'tmp'],
  ability: 'example/run-task',
  abilityInput: { prompt: 'Cook.' },
  workload: { schema: 'wp-codebox/workload/v1', id: 'cook-workload', label: 'Cook workload' },
  toolPolicy: { schema: 'wp-codebox/tool-policy/v1', tools: { workspace_read: true } },
  abilityTools: [{ name: 'example_tool' }],
  runtimeOutputProjections: { packet_url: 'metadata.artifacts.packet.url' },
  callbackData: { initial: { seed: 'value' } },
  evidenceProjections: [{ operation: 'workspacePublish', outputs: { pr_url: 'result.pr.url' } }],
  artifactSlots: [{ name: 'packet', required: true }],
  artifactDeclarations: [{ schema: 'wp-codebox/artifact-declaration/v1', name: 'packet', required: true }],
  transcriptSlots: [{ name: 'main', required: true }],
  runtimeInvocation: { operations: ['workspaceCommand'] },
});

assert.equal(genericConfig.runtime_id, 'wp-codebox');
assert.equal(genericConfig.runtime_profile, 'example-agent-ci');
assert.deepEqual(genericConfig.runtime_component_paths, { agent_runtime: '/workspace/components/example-runtime' });
assert.deepEqual(genericConfig.ignored_workspace_paths, ['.cache', 'tmp']);
assert.deepEqual(genericConfig.runtime_task, { ability: 'example/run-task', input: { prompt: 'Cook.' } });
assert.deepEqual(genericConfig.workload, { schema: 'wp-codebox/workload/v1', id: 'cook-workload', label: 'Cook workload' });
assert.deepEqual(genericConfig.sandbox_tool_policy, { schema: 'wp-codebox/tool-policy/v1', tools: { workspace_read: true } });
assert.deepEqual(genericConfig.ability_requirements, ['example/run-task', 'example/read-state']);
assert.deepEqual(genericConfig.ability_tools, [{ name: 'example_tool' }]);
assert.deepEqual(genericConfig.runtime_output_projections, { packet_url: 'metadata.artifacts.packet.url' });
assert.deepEqual(genericConfig.callback_data, { initial: { seed: 'value' } });
assert.deepEqual(genericConfig.evidence_projections, [{ operation: 'workspacePublish', outputs: { pr_url: 'result.pr.url' } }]);
assert.deepEqual(genericConfig.artifact_slots, [{ name: 'packet', required: true }]);
assert.deepEqual(genericConfig.artifact_declarations, [{ schema: 'wp-codebox/artifact-declaration/v1', name: 'packet', required: true }]);
assert.deepEqual(genericConfig.transcript_slots, [{ name: 'main', required: true }]);
assert.deepEqual(genericConfig.provider_runtime_invocation, { operations: ['workspaceCommand'] });

const capabilityBundleConfig = runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
  runtimeProfile: runtimeProfile.id,
  runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
  ability: 'example/run-task',
  capabilityBundles: ['worktree_pr_iteration'],
  runtimeInvocation: { operations: { workspaceCommand: { config: { timeout_ms: 30000 } } } },
});

assert.deepEqual(capabilityBundleConfig.capability_bundles, ['worktree_pr_iteration']);
assert.deepEqual(capabilityBundleConfig.tool_presets, ['runner_workspace', 'publication']);
assert.deepEqual(capabilityBundleConfig.workspace_tools, {
  readonly: ['workspace_ls', 'workspace_read', 'workspace_git_status'],
  readwrite: ['workspace_run', 'workspace_write', 'workspace_edit', 'workspace_apply_patch', 'workspace_delete', 'workspace_git_add'],
});
assert.deepEqual(capabilityBundleConfig.publication_tools, ['publication_prepare', 'publication_publish', 'publication_status']);
assert.deepEqual(capabilityBundleConfig.provider_runtime_invocation, {
  operations: {
    workspaceCommand: { config: { timeout_ms: 30000 } },
    workspaceCapture: true,
    workspacePublish: true,
    artifactHandoff: true,
    toolCallTranscriptRecord: true,
  },
});
assert.doesNotMatch(JSON.stringify(capabilityBundleConfig.provider_runtime_invocation), /wp-codebox\/runner-workspace-command/);

assert.deepEqual(
  runtimeAgentCi.runtimeAgentCiTaskFromRequest(
    {},
    { ability: 'example/process', input: { source: 'artifact.json' } },
    { mode: 'typed' }
  ),
  { ability: 'example/process', input: { source: 'artifact.json', mode: 'typed' } }
);

const genericBundleConfig = runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
  runtimeProfile: runtimeProfile.id,
  runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
  runtimeExecution: {
    kind: 'bundle',
    source: '/workspace/example-repo/bundles/example-agent',
    workflow: { name: 'materialize-artifact' },
    input: { prompt: 'Cook a packet.' },
  },
});

assert.equal(genericBundleConfig.runtime_task.ability, 'runtime/run-agent-bundle');
assert.equal(genericBundleConfig.runtime_execution.schema, 'homeboy/runtime-execution/v1');
assert.equal(genericBundleConfig.runtime_task.input.source, '/workspace/example-repo/bundles/example-agent');
assert.deepEqual(genericBundleConfig.runtime_task.input.workflow, { name: 'materialize-artifact' });
assert.equal(genericBundleConfig.runtime_task.input.prompt, 'Cook a packet.');
assert.equal(genericBundleConfig.runtime_execution.kind, 'bundle');

const emptyCodeboxDtoConfig = runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
  runtimeProfile: runtimeProfile.id,
  runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
  ability: 'example/run-task',
  toolPolicy: {},
  workload: {},
});
assert.equal(emptyCodeboxDtoConfig.sandbox_tool_policy, undefined);
assert.equal(emptyCodeboxDtoConfig.workload, undefined);

const genericPackageConfig = runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
  runtimeProfile: runtimeProfile.id,
  runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
  runtimeExecution: {
    bundle: { path: '/workspace/example-repo/packages/example-agent' },
    input: { prompt: 'Cook from package.' },
  },
  runtimeOutputProjections: { package_pr_url: 'metadata.publication.pr_url' },
  evidenceProjections: [{ operation: 'workspacePublish', outputs: { pr_url: 'result.pr.url' } }],
});

assert.equal(genericPackageConfig.runtime_task.ability, 'runtime/run-agent-bundle');
assert.equal(genericPackageConfig.runtime_task.input.source, '/workspace/example-repo/packages/example-agent');
assert.equal(genericPackageConfig.runtime_task.input.prompt, 'Cook from package.');
assert.deepEqual(genericPackageConfig.runtime_output_projections, { package_pr_url: 'metadata.publication.pr_url' });
assert.deepEqual(genericPackageConfig.evidence_projections, [{ operation: 'workspacePublish', outputs: { pr_url: 'result.pr.url' } }]);

const genericWorkflowConfig = runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
  runtimeProfile: runtimeProfile.id,
  runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
  runtimeExecution: {
    kind: 'workflow',
    workflow: { path: '.ci/workflows/materialize.json' },
    input: { dry_run: true },
  },
});

assert.equal(genericWorkflowConfig.runtime_task.ability, 'runtime/execute-workflow');
assert.deepEqual(genericWorkflowConfig.runtime_task.input.workflow, { path: '.ci/workflows/materialize.json' });
assert.equal(genericWorkflowConfig.runtime_task.input.dry_run, true);

const directAbilityConfig = runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
  runtimeProfile: runtimeProfile.id,
  runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
  runtimeExecution: {
    kind: 'ability',
    ability: 'example/direct-operation',
    input: { dry_run: false },
  },
});

assert.deepEqual(directAbilityConfig.runtime_task, { ability: 'example/direct-operation', input: { dry_run: false } });

assert.throws(
  () => runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
    runtimeProfile: 'limited-runtime',
    runtimeProfiles: {
      'limited-runtime': {
        id: 'limited-runtime',
        runtime_task_ability: 'example/run-task',
        runtime_bundle_ability: 'runtime/run-agent-bundle',
        capabilities: ['ability_execution'],
        runtime_execution_contracts: {
          bundle: {
            ability_field: 'runtime_bundle_ability',
            required_capabilities: ['agent_bundle_execution'],
          },
        },
      },
    },
    runtimeExecution: { kind: 'bundle', source: 'bundles/example' },
  }),
  /runtime_execution kind bundle requires unsupported provider capabilities: agent_bundle_execution/
);

const genericRequest = runtimeAgentCi.runtimeAgentCiAbilityTaskRequest({
  taskId: 'task-1',
  backend: 'wp-codebox',
  runtime: 'wp-codebox',
  runtimeProfile: runtimeProfile.id,
  runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
  ability: 'example/run-task',
  abilityInput: { prompt: 'Cook.' },
  expectedArtifacts: ['packet'],
});

assert.equal(genericRequest.schema, 'homeboy/agent-task-request/v1');
assert.equal(genericRequest.executor.backend, 'wp-codebox');
assert.equal(genericRequest.executor.runtime, 'wp-codebox');
assert.deepEqual(genericRequest.expected_artifacts, ['packet']);
assert.deepEqual(genericRequest.executor.config.runtime_task, { ability: 'example/run-task', input: { prompt: 'Cook.' } });

assert.deepEqual(
  runtimeAgentCi.runtimeAgentCiAbilityTaskRequest({
    taskId: 'equivalent-task',
    backend: 'wp-codebox',
    runtime: 'wp-codebox',
    runtimeProfile: runtimeProfile.id,
    runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
    ability: 'example/run-task',
    abilityInput: { prompt: 'Cook.' },
    expectedArtifacts: ['packet'],
  }),
  genericAgentTaskRequest({
    task_id: 'equivalent-task',
    instructions: '',
    inputs: {},
    runnerSpec: runtimeAgentCi.runtimeAgentCiRunnerSpec({
      backend: 'wp-codebox',
      runtime: 'wp-codebox',
      runtimeProfile: runtimeProfile.id,
      runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
      ability: 'example/run-task',
      abilityInput: { prompt: 'Cook.' },
      expectedArtifacts: ['packet'],
    }),
  })
);

assert.deepEqual(
  runtimeAgentCi.runtimeAgentCiPlan({
    planId: 'equivalent-plan',
    tasks: [genericRequest],
    options: { concurrency: 1 },
    metadata: { preset: 'runtime-agent-ci' },
  }),
  genericAgentTaskPlan({
    plan_id: 'equivalent-plan',
    tasks: [genericRequest],
    options: { concurrency: 1 },
    metadata: { preset: 'runtime-agent-ci' },
  })
);

console.log('runtime agent CI contract smoke passed');
