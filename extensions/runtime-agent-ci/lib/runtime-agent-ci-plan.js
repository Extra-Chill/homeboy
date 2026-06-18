'use strict';

const {
  AGENT_TASK_RUNNER_SPEC_SCHEMA,
  agentTaskRequestFromRunnerSpec,
  agentTaskRunnerSpec,
  validateAgentTaskRunnerSpec,
} = require('../../agent-runtimes/lib/agent-task-runner-contract');

const AGENT_TASK_PLAN_SCHEMA = 'homeboy/agent-task-plan/v1';
const AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const RUNTIME_AGENT_CI_RUNTIME_PROFILE_ID = 'runtime-agent-ci';

function runtimeAgentCiRuntimeTaskRequest(options = {}, context = {}) {
  const taskId = requiredString(options.taskId || options.task_id, 'taskId');
  const runtimeProfile = resolveRuntimeAgentCiRuntimeProfile(options);
  const runtimeTaskInput = stripUndefined({
    ...(options.runtimeTaskInput || options.runtime_task_input || {}),
  });

  return runtimeAgentCiAbilityTaskRequest({
    ...options,
    taskId,
    ability: options.ability || runtimeProfile.runtime_task_ability,
    abilityInput: runtimeTaskInput,
  }, context);
}

function runtimeAgentCiAbilityTaskRequest(options = {}, context = {}) {
  const taskId = requiredString(options.taskId || options.task_id, 'taskId');
  const ability = requiredString(options.ability, 'ability');
  const runnerSpec = runtimeAgentCiRunnerSpec({
    ...options,
    ability,
  }, context);
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

function runtimeAgentCiRunnerSpec(options = {}, context = {}) {
  const taskExecutorConfig = context.taskExecutorConfig || runtimeAgentCiTaskExecutorConfig;
  const config = taskExecutorConfig(options);
  return agentTaskRunnerSpec({
    backend: options.backend || options.runtimeProvider || options.runtime_provider || 'codebox',
    config,
    secret_env: normalizeArray(config.secret_env),
    task_timeout_seconds: config.task_timeout_seconds || options.taskTimeoutSeconds || options.task_timeout_seconds || 900,
    limits: options.limits,
    expected_artifacts: options.expectedArtifacts || options.expected_artifacts,
  });
}

function runtimeAgentCiTaskExecutorConfig(options = {}) {
  const runtimeProfile = resolveRuntimeAgentCiRuntimeProfile(options);
  const runtimeTaskInput = options.abilityInput || options.ability_input || {};
  const runtimeTask = stripUndefined({
    ability: options.ability || runtimeProfile.runtime_task_ability,
    input: runtimeTaskInput,
  });
  return stripUndefined({
    ...(options.config || {}),
    provider: options.provider,
    model: options.model,
    runtime_provider: options.runtimeProvider || options.runtime_provider,
    runtime_profile: runtimeProfile.id,
    runtime_profiles: runtimeProfilesForOptions(options, runtimeProfile),
    runtime_component_paths: options.runtimeComponentPaths || options.runtime_component_paths,
    component_contracts: options.componentContracts || options.component_contracts,
    homeboy_extensions: options.homeboyExtensions || options.homeboy_extensions,
    agent_bundles: options.agentBundles || options.agent_bundles,
    ignored_workspace_paths: options.ignoredWorkspacePaths || options.ignored_workspace_paths,
    runtime_task: runtimeTask,
    ability_tools: options.abilityTools || options.ability_tools,
    ability_requirements: options.abilityRequirements || options.ability_requirements || runtimeProfile.ability_requirements,
    artifact_slots: options.artifactSlots || options.artifact_slots,
    transcript_slots: options.transcriptSlots || options.transcript_slots,
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
}

function resolveRuntimeAgentCiRuntimeProfile(options = {}) {
  const runtimeProfiles = options.runtimeProfiles || options.runtime_profiles || options.config?.runtime_profiles || options.config?.runtimeProfiles || {};
  const requestedProfile = options.runtimeProfile || options.runtime_profile || options.config?.runtime_profile || options.config?.runtimeProfile || RUNTIME_AGENT_CI_RUNTIME_PROFILE_ID;
  const profile = runtimeProfiles[requestedProfile] || options.runtimeProfileConfig || options.runtime_profile_config;
  if (!profile || typeof profile !== 'object' || Array.isArray(profile)) {
    throw new Error(`runtime profile ${requestedProfile} is not configured.`);
  }

  const id = requiredString(profile.id || requestedProfile, 'runtime profile id');
  return {
    ...profile,
    id,
    runtime_task_ability: requiredString(
      options.runtimeTaskAbility || options.runtime_task_ability || profile.runtime_task_ability,
      'runtime task ability'
    ),
  };
}

function runtimeProfilesForOptions(options, runtimeProfile) {
  return {
    ...(options.config?.runtime_profiles || options.config?.runtimeProfiles || {}),
    ...(options.runtimeProfiles || options.runtime_profiles || {}),
    [runtimeProfile.id]: runtimeProfile,
  };
}

function runtimeAgentCiPlan(options = {}) {
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
  RUNTIME_AGENT_CI_RUNTIME_PROFILE_ID,
  agentTaskRequestFromRunnerSpec,
  runtimeAgentCiAbilityTaskRequest,
  runtimeAgentCiPlan,
  runtimeAgentCiRunnerSpec,
  runtimeAgentCiRuntimeTaskRequest,
  runtimeAgentCiTaskExecutorConfig,
  resolveRuntimeAgentCiRuntimeProfile,
  validateAgentTaskRunnerSpec,
};
