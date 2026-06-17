'use strict';

/**
 * Internal dependencies
 */
const {
  datamachineAgentCiCodeboxExecutorConfig,
} = require('./datamachine-agent-ci-codebox-adapter');
const {
  AGENT_TASK_RUNNER_SPEC_SCHEMA,
  agentTaskRequestFromRunnerSpec,
  agentTaskRunnerSpec,
  validateAgentTaskRunnerSpec,
} = require('./agent-task-runner-spec');

const AGENT_TASK_PLAN_SCHEMA = 'homeboy/agent-task-plan/v1';
const AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY = 'datamachine/run-agent-bundle';

function datamachineAgentCiBundleTaskRequest(options = {}) {
  const taskId = requiredString(options.taskId || options.task_id, 'taskId');
  const source = requiredString(options.source || options.bundle, 'source');
  const agentSlug = requiredString(options.agentSlug || options.agent_slug, 'agentSlug');
  const pipelineSlug = requiredString(options.pipelineSlug || options.pipeline_slug, 'pipelineSlug');
  const flowSlug = requiredString(options.flowSlug || options.flow_slug, 'flowSlug');

  const runtimeTaskInput = stripUndefined({
    source,
    agent_slug: agentSlug,
    pipeline_slug: pipelineSlug,
    flow_slug: flowSlug,
    target_repo: options.targetRepo || options.target_repo,
    prompt: options.prompt || '',
    wait_for_completion: options.waitForCompletion ?? options.wait_for_completion ?? true,
    success_requires_pr: options.successRequiresPr ?? options.success_requires_pr,
    success_completion_outcomes: options.successCompletionOutcomes || options.success_completion_outcomes,
    artifact_outputs: options.artifactOutputs || options.artifact_outputs,
    flow_step_patches: options.flowStepPatches || options.flow_step_patches,
    tool_recorders: options.toolRecorders || options.tool_recorders,
    engine_data_outputs: options.engineDataOutputs || options.engine_data_outputs,
    transcript_artifact_name: options.transcriptArtifactName || options.transcript_artifact_name,
    artifacts: options.artifactsPath || options.artifacts,
    max_turns: options.maxTurns || options.max_turns,
    step_budget: options.stepBudget || options.step_budget,
    time_budget_ms: options.timeBudgetMs || options.time_budget_ms,
    complexity_policy: options.complexityPolicy || options.complexity_policy,
    ...stripUndefined(options.runtimeTaskInput || options.runtime_task_input || {}),
  });

  return datamachineAgentCiAbilityTaskRequest({
    ...options,
    taskId,
    ability: DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY,
    abilityInput: runtimeTaskInput,
  });
}

function datamachineAgentCiAbilityTaskRequest(options = {}) {
  const taskId = requiredString(options.taskId || options.task_id, 'taskId');
  const ability = requiredString(options.ability, 'ability');
  const runnerSpec = datamachineAgentCiRunnerSpec({
    ...options,
    ability,
  });
  const runnerRequest = agentTaskRequestFromRunnerSpec({ runnerSpec });

  return stripUndefined({
    schema: AGENT_TASK_REQUEST_SCHEMA,
    task_id: taskId,
    group_key: options.groupKey || options.group_key,
    parent_plan_id: options.parentPlanId || options.parent_plan_id,
    cwd: options.cwd,
    repo: options.repo,
    workspace: options.workspace,
    executor: runnerRequest.executor,
    instructions: options.instructions || '',
    inputs: {
      ...(options.inputs || {}),
      ...(options.title ? { title: options.title } : {}),
    },
    limits: runnerRequest.limits,
    expected_artifacts: runnerRequest.expected_artifacts,
  });
}

function datamachineAgentCiRunnerSpec(options = {}) {
  const config = datamachineAgentCiTaskExecutorConfig(options);
  return agentTaskRunnerSpec({
    backend: options.backend || 'codebox',
    config,
    secret_env: normalizeArray(config.secret_env),
    task_timeout_seconds: config.task_timeout_seconds || options.taskTimeoutSeconds || options.task_timeout_seconds || 900,
    limits: options.limits,
    expected_artifacts: options.expectedArtifacts || options.expected_artifacts,
  });
}

function datamachineAgentCiTaskExecutorConfig(options = {}) {
  const runtimeTaskInput = options.abilityInput || options.ability_input || {};
  const runtimeTask = stripUndefined({
    ability: options.ability,
    input: runtimeTaskInput,
  });
  const config = stripUndefined({
    ...(options.config || {}),
    provider: options.provider,
    model: options.model,
    runtime_component_paths: options.runtimeComponentPaths || options.runtime_component_paths,
    component_contracts: options.componentContracts || options.component_contracts,
    homeboy_extensions: options.homeboyExtensions || options.homeboy_extensions,
    agent_bundles: options.agentBundles || options.agent_bundles,
    runtime_task: runtimeTask,
    ability_tools: options.abilityTools || options.ability_tools,
    structured_artifacts: options.structuredArtifacts || options.structured_artifacts,
    task_timeout_seconds: options.taskTimeoutSeconds || options.task_timeout_seconds,
    max_turns: options.maxTurns || options.max_turns,
    provider_plugin_paths: options.providerPluginPaths || options.provider_plugin_paths,
    secret_env: options.secretEnv || options.secret_env,
    runtime_env: options.runtimeEnv || options.runtime_env,
    runtime_config_mounts: options.runtimeConfigMounts || options.runtime_config_mounts,
    runtime_state_mounts: options.runtimeStateMounts || options.runtime_state_mounts,
    runtime_id: options.runtimeId || options.runtime_id,
    runtime_bin: options.runtimeBin || options.runtime_bin,
  });

  return datamachineAgentCiCodeboxExecutorConfig(config);
}

function datamachineAgentCiPlan(options = {}) {
  const planId = requiredString(options.planId || options.plan_id, 'planId');
  const tasks = normalizeArray(options.tasks);
  if (tasks.length === 0) {
    throw new Error('tasks must contain at least one task request.');
  }
  return stripUndefined({
    schema: AGENT_TASK_PLAN_SCHEMA,
    plan_id: planId,
    tasks,
    options: options.options,
    metadata: options.metadata,
  });
}

function normalizeArray(value) {
  return Array.isArray(value) ? value.filter(Boolean) : [];
}

function requiredString(value, name) {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`${name} is required.`);
  }
  return value;
}

function stripUndefined(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return value;
  }
  return Object.fromEntries(
    Object.entries(value).filter(([, entry]) => entry !== undefined)
  );
}

module.exports = {
  AGENT_TASK_PLAN_SCHEMA,
  AGENT_TASK_REQUEST_SCHEMA,
  AGENT_TASK_RUNNER_SPEC_SCHEMA,
  DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY,
  agentTaskRequestFromRunnerSpec,
  datamachineAgentCiAbilityTaskRequest,
  datamachineAgentCiBundleTaskRequest,
  datamachineAgentCiPlan,
  datamachineAgentCiRunnerSpec,
  datamachineAgentCiTaskExecutorConfig,
  validateAgentTaskRunnerSpec,
};
