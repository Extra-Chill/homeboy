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
assert.equal(bundleTask.executor.config.runtime_requirements.component_path_defaults.path_aliases.agent_runtime.includes('runtime_component:data_machine'), true);
assert.equal(bundleTask.limits.task_timeout_seconds, 1200);
assert.deepEqual(bundleTask.expected_artifacts, ['datamachine-transcript', 'ConceptPacket']);

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
  abilityInput: { artifact: '{{outputs.static_site_candidate}}' },
  instructions: 'Validate the artifact.',
});

assert.equal(abilityTask.executor.config.runtime_task.ability, 'example/validate-artifact');
assert.deepEqual(abilityTask.executor.config.runtime_task.input, {
  artifact: '{{outputs.static_site_candidate}}',
});

const plan = datamachineAgentCiPlan({
  planId: 'site-generation-loop-123',
  tasks: [bundleTask, abilityTask],
  options: { max_concurrency: 1 },
  metadata: { source: 'example' },
});

assert.equal(plan.schema, 'homeboy/agent-task-plan/v1');
assert.equal(plan.plan_id, 'site-generation-loop-123');
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
