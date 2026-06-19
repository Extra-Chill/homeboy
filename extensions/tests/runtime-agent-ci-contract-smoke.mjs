#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const runtimeAgentCi = require(path.join(repoRoot, 'runtime-agent-ci/index.js'));

const genericBoundaryTerms = /Data Machine|DataMachine|datamachine|data-machine|wp-site-generator|WPSG|site-generator|site generator/;
const genericFiles = [
  'runtime-agent-ci/index.js',
  'runtime-agent-ci/lib/runtime-agent-ci-plan.js',
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
  component_path_defaults: {
    contract_slug_map: { 'example-runtime': 'agent_runtime' },
    path_aliases: { agent_runtime: ['contract:agent_runtime'] },
  },
  ability_requirements: ['example/run-task', 'example/read-state'],
};

const genericConfig = runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
  runtimeProvider: 'codebox',
  provider: 'example-provider',
  model: 'example-model',
  runtimeProfile: runtimeProfile.id,
  runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
  runtimeComponentPaths: { agent_runtime: '/workspace/components/example-runtime' },
  ignoredWorkspacePaths: ['.cache', 'tmp'],
  ability: 'example/run-task',
  abilityInput: { prompt: 'Cook.' },
  abilityTools: [{ name: 'example_tool' }],
  runtimeOutputProjections: { packet_url: 'metadata.artifacts.packet.url' },
  callbackData: { initial: { seed: 'value' } },
  evidenceProjections: [{ operation: 'workspacePublish', outputs: { pr_url: 'result.pr.url' } }],
  artifactSlots: [{ name: 'packet', required: true }],
  transcriptSlots: [{ name: 'main', required: true }],
  runtimeInvocation: { operations: ['workspaceCommand'] },
});

assert.equal(genericConfig.runtime_provider, 'codebox');
assert.equal(genericConfig.runtime_profile, 'example-agent-ci');
assert.deepEqual(genericConfig.runtime_component_paths, { agent_runtime: '/workspace/components/example-runtime' });
assert.deepEqual(genericConfig.ignored_workspace_paths, ['.cache', 'tmp']);
assert.deepEqual(genericConfig.runtime_task, { ability: 'example/run-task', input: { prompt: 'Cook.' } });
assert.deepEqual(genericConfig.ability_requirements, ['example/run-task', 'example/read-state']);
assert.deepEqual(genericConfig.ability_tools, [{ name: 'example_tool' }]);
assert.deepEqual(genericConfig.runtime_output_projections, { packet_url: 'metadata.artifacts.packet.url' });
assert.deepEqual(genericConfig.callback_data, { initial: { seed: 'value' } });
assert.deepEqual(genericConfig.evidence_projections, [{ operation: 'workspacePublish', outputs: { pr_url: 'result.pr.url' } }]);
assert.deepEqual(genericConfig.artifact_slots, [{ name: 'packet', required: true }]);
assert.deepEqual(genericConfig.transcript_slots, [{ name: 'main', required: true }]);
assert.deepEqual(genericConfig.provider_runtime_invocation, { operations: ['workspaceCommand'] });

assert.deepEqual(
  runtimeAgentCi.runtimeAgentCiTaskFromRequest(
    {},
    { ability: 'example/process', input: { source: 'artifact.json' } },
    { mode: 'typed' }
  ),
  { ability: 'example/process', input: { source: 'artifact.json', mode: 'typed' } }
);
assert.deepEqual(
  runtimeAgentCi.runtimeAgentCiFirstNonEmptyObject({}, { legacy: true }),
  { legacy: true }
);
assert.deepEqual(
  runtimeAgentCi.runtimeAgentCiFirstNonEmptyArray([], [{ legacy: true }]),
  [{ legacy: true }]
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
assert.equal(genericBundleConfig.runtime_task.input.source, '/workspace/example-repo/bundles/example-agent');
assert.deepEqual(genericBundleConfig.runtime_task.input.workflow, { name: 'materialize-artifact' });
assert.equal(genericBundleConfig.runtime_task.input.prompt, 'Cook a packet.');
assert.equal(genericBundleConfig.runtime_execution.kind, 'bundle');
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

const genericRequest = runtimeAgentCi.runtimeAgentCiAbilityTaskRequest({
  taskId: 'task-1',
  backend: 'codebox',
  runtimeProvider: 'codebox',
  runtimeProfile: runtimeProfile.id,
  runtimeProfiles: { [runtimeProfile.id]: runtimeProfile },
  ability: 'example/run-task',
  abilityInput: { prompt: 'Cook.' },
  expectedArtifacts: ['packet'],
});

assert.equal(genericRequest.schema, 'homeboy/agent-task-request/v1');
assert.equal(genericRequest.executor.backend, 'codebox');
assert.deepEqual(genericRequest.expected_artifacts, ['packet']);
assert.deepEqual(genericRequest.executor.config.runtime_task, { ability: 'example/run-task', input: { prompt: 'Cook.' } });

console.log('runtime agent CI contract smoke passed');
