#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const runtimeAgentCi = require(path.join(repoRoot, 'runtime-agent-ci/index.js'));
const datamachineAgentCi = require(path.join(repoRoot, 'datamachine-agent-ci/index.js'));

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
  artifactSlots: [{ name: 'packet', required: true }],
  transcriptSlots: [{ name: 'main', required: true }],
});

assert.equal(genericConfig.runtime_provider, 'codebox');
assert.equal(genericConfig.runtime_profile, 'example-agent-ci');
assert.deepEqual(genericConfig.runtime_component_paths, { agent_runtime: '/workspace/components/example-runtime' });
assert.deepEqual(genericConfig.ignored_workspace_paths, ['.cache', 'tmp']);
assert.deepEqual(genericConfig.runtime_task, { ability: 'example/run-task', input: { prompt: 'Cook.' } });
assert.deepEqual(genericConfig.ability_requirements, ['example/run-task', 'example/read-state']);
assert.deepEqual(genericConfig.ability_tools, [{ name: 'example_tool' }]);
assert.deepEqual(genericConfig.artifact_slots, [{ name: 'packet', required: true }]);
assert.deepEqual(genericConfig.transcript_slots, [{ name: 'main', required: true }]);

const genericRequest = runtimeAgentCi.runtimeAgentCiAbilityTaskRequest({
  taskId: 'task-1',
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

const adapterConfig = datamachineAgentCi.datamachineAgentCiTaskExecutorConfig({
  taskId: 'task-2',
  source: 'bundle',
  agentSlug: 'agent',
  pipelineSlug: 'pipeline',
  flowSlug: 'flow',
});

assert.equal(adapterConfig.runtime_profile, datamachineAgentCi.DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID);
assert.equal(
  adapterConfig.runtime_profiles[datamachineAgentCi.DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID].runtime_task_ability,
  datamachineAgentCi.DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY
);
assert.deepEqual(
  adapterConfig.runtime_profiles[datamachineAgentCi.DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID].component_path_defaults,
  datamachineAgentCi.DATAMACHINE_AGENT_CI_COMPONENT_PATH_DEFAULTS
);
assert.deepEqual(
  adapterConfig.runtime_profiles[datamachineAgentCi.DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID].ability_requirements,
  datamachineAgentCi.DATAMACHINE_AGENT_CI_ABILITY_REQUIREMENTS
);

console.log('runtime agent CI contract smoke passed');
