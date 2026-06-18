'use strict';

const assert = require('node:assert/strict');

const {
  AGENT_TASK_RUNNER_SPEC_SCHEMA,
  agentTaskRequestFromRunnerSpec,
  datamachineAgentCiAbilityTaskRequest,
  datamachineAgentCiBundleTaskRequest,
  datamachineAgentCiPlan,
  datamachineAgentCiRunnerSpec,
  validateAgentTaskRunnerSpec,
} = require('../lib/datamachine-agent-ci-plan');
const datamachineAgentCi = require('../../datamachine-agent-ci');

assert.equal(
  datamachineAgentCi.DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY,
  'datamachine/run-agent-bundle'
);
assert.equal(
  datamachineAgentCi.DATAMACHINE_AGENT_CI_COMPONENT_PATH_DEFAULTS.path_aliases.agent_runtime.includes('runtime_component:data_machine'),
  true
);
assert.equal(datamachineAgentCi.DATAMACHINE_AGENT_CI_RUNTIME_PROFILE.id, 'datamachine-agent-ci');
assert.equal(datamachineAgentCi.DATAMACHINE_AGENT_CI_RUNTIME_PROFILE.ability_requirements.includes('datamachine/run-agent-bundle'), true);

const bundleTask = datamachineAgentCiBundleTaskRequest({
  taskId: 'concept-agent',
  groupKey: 'site-generation-loop',
  parentPlanId: 'site-generation-loop-123',
  title: 'Generate concept packet',
  source: '/workspace/example-repo/bundles/concept-agent',
  agentSlug: 'concept-agent',
  pipelineSlug: 'concept-pipeline',
  flowSlug: 'concept-flow',
  targetRepo: 'example/repo',
  prompt: 'Generate a typed concept packet.',
  artifactOutputs: {
    concept_packet: {
      schema: 'example/ConceptPacket/v1',
      path: '/artifacts/ConceptPacket.json',
    },
  },
  structuredArtifacts: [
    {
      schema: 'wp-codebox/structured-artifact/v1',
      name: 'concept_packet',
      payload_schema: 'example/ConceptPacket/v1',
    },
  ],
  runtimeComponentPaths: {
    agent_runtime: '.ci/data-machine',
    agent_runtime_tools: '.ci/data-machine-code',
  },
  componentContracts: [{ slug: 'example-repo', path: '.', activate: true }],
  homeboyExtensions: '.ci/homeboy-extensions/wordpress',
  agentBundles: [{ source: '/workspace/example-repo/bundles/concept-agent', slug: 'concept-agent' }],
  secretEnv: ['AI_PROVIDER_OPENAI_API_KEY'],
  expectedArtifacts: ['datamachine-transcript', 'ConceptPacket'],
  taskTimeoutSeconds: 1200,
});

assert.equal(bundleTask.schema, 'homeboy/agent-task-request/v1');
assert.equal(bundleTask.executor.backend, 'codebox');
assert.deepEqual(bundleTask.executor.secret_env, ['AI_PROVIDER_OPENAI_API_KEY']);
assert.equal(bundleTask.executor.config.runtime_task.ability, 'datamachine/run-agent-bundle');
assert.deepEqual(bundleTask.executor.config.runtime_task.input, {
  source: '/workspace/example-repo/bundles/concept-agent',
  agent_slug: 'concept-agent',
  pipeline_slug: 'concept-pipeline',
  flow_slug: 'concept-flow',
  target_repo: 'example/repo',
  prompt: 'Generate a typed concept packet.',
  wait_for_completion: true,
  artifact_outputs: {
    concept_packet: {
      schema: 'example/ConceptPacket/v1',
      path: '/artifacts/ConceptPacket.json',
    },
  },
});
assert.equal(bundleTask.executor.config.runtime_profile, 'datamachine-agent-ci');
assert.equal(bundleTask.executor.config.runtime_profiles['datamachine-agent-ci'].component_path_defaults.path_aliases.agent_runtime.includes('runtime_component:data_machine'), true);
assert.equal(bundleTask.limits.task_timeout_seconds, 1200);
assert.deepEqual(bundleTask.expected_artifacts, ['datamachine-transcript', 'ConceptPacket']);

const customRuntimeProfile = {
  schema: 'homeboy/runtime-profile/v1',
  id: 'example-agent-ci',
  runtime_task_ability: 'example/run-agent-bundle',
  component_path_defaults: {
    contract_slug_map: {
      'example-agents': 'agent_runtime',
      'example-tools': 'agent_runtime_tools',
    },
    path_aliases: {
      agent_runtime: ['contract:agent_runtime'],
      agent_runtime_tools: ['contract:agent_runtime_tools'],
    },
  },
  ability_requirements: ['example/run-agent-bundle'],
};
const customBundleTask = datamachineAgentCiBundleTaskRequest({
  taskId: 'example-agent',
  source: '/workspace/example-repo/bundles/example-agent',
  agentSlug: 'example-agent',
  pipelineSlug: 'example-pipeline',
  flowSlug: 'example-flow',
  runtimeProfile: 'example-agent-ci',
  runtimeProfiles: { 'example-agent-ci': customRuntimeProfile },
  componentContracts: [
    { slug: 'example-agents', path: '/components/example-agents' },
    { slug: 'example-tools', path: '/components/example-tools' },
  ],
});

assert.equal(customBundleTask.executor.config.runtime_profile, 'example-agent-ci');
assert.equal(customBundleTask.executor.config.runtime_task.ability, 'example/run-agent-bundle');
assert.deepEqual(customBundleTask.executor.config.runtime_profiles['example-agent-ci'].component_path_defaults.contract_slug_map, {
  'example-agents': 'agent_runtime',
  'example-tools': 'agent_runtime_tools',
});
assert.deepEqual(customBundleTask.executor.config.component_contracts, [
  { slug: 'example-agents', path: '/components/example-agents' },
  { slug: 'example-tools', path: '/components/example-tools' },
]);

const bundleRunnerSpec = datamachineAgentCiRunnerSpec({
  taskId: 'concept-agent',
  source: '/workspace/example-repo/bundles/concept-agent',
  agentSlug: 'concept-agent',
  pipelineSlug: 'concept-pipeline',
  flowSlug: 'concept-flow',
  provider: 'openai',
  secretEnv: ['AI_PROVIDER_OPENAI_API_KEY'],
  expectedArtifacts: ['datamachine-transcript'],
});

assert.equal(bundleRunnerSpec.schema, AGENT_TASK_RUNNER_SPEC_SCHEMA);
assert.equal(bundleRunnerSpec.executor.backend, 'codebox');
assert.equal(bundleRunnerSpec.executor.config.provider, 'openai');
assert.deepEqual(bundleRunnerSpec.executor.secret_env, ['AI_PROVIDER_OPENAI_API_KEY']);
assert.deepEqual(agentTaskRequestFromRunnerSpec({ runnerSpec: bundleRunnerSpec }), {
  executor: bundleRunnerSpec.executor,
  limits: { task_timeout_seconds: 900 },
  expected_artifacts: ['datamachine-transcript'],
});
assert.equal(validateAgentTaskRunnerSpec(bundleRunnerSpec), bundleRunnerSpec);

const abilityTask = datamachineAgentCiAbilityTaskRequest({
  taskId: 'validate-artifact',
  ability: 'example/validate-artifact',
  abilityInput: { artifact: '{{outputs.example_review}}' },
  instructions: 'Validate the artifact.',
});

assert.equal(abilityTask.executor.config.runtime_task.ability, 'example/validate-artifact');
assert.deepEqual(abilityTask.executor.config.runtime_task.input, {
  artifact: '{{outputs.example_review}}',
});

const plan = datamachineAgentCiPlan({
  planId: 'example-agent-loop-123',
  tasks: [bundleTask, abilityTask],
  options: { max_concurrency: 1 },
  metadata: { source: 'example' },
});

assert.equal(plan.schema, 'homeboy/agent-task-plan/v1');
assert.equal(plan.plan_id, 'example-agent-loop-123');
assert.equal(plan.tasks.length, 2);
assert.deepEqual(plan.options, { max_concurrency: 1 });

assert.throws(
  () => datamachineAgentCiBundleTaskRequest({ taskId: 'missing-source' }),
  /source is required/
);
assert.throws(
  () => datamachineAgentCiPlan({ planId: 'empty-plan', tasks: [] }),
  /tasks must contain at least one task request/
);
assert.throws(
  () => validateAgentTaskRunnerSpec({ schema: AGENT_TASK_RUNNER_SPEC_SCHEMA }),
  /runner spec executor is required/
);
