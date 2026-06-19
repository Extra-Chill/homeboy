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
  const runtimeExecution = normalizeRuntimeExecutionDescriptor(options.runtimeExecution || options.runtime_execution, runtimeProfile);
  const runtimeTaskInput = runtimeExecution?.input || stripUndefined({
    ...(options.runtimeTaskInput || options.runtime_task_input || {}),
  });

  return runtimeAgentCiAbilityTaskRequest({
    ...options,
    taskId,
    ability: runtimeExecution?.ability || options.ability || runtimeProfile.runtime_task_ability,
    abilityInput: runtimeTaskInput,
    runtimeExecution: runtimeExecution || options.runtimeExecution || options.runtime_execution,
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
    backend: options.backend || options.runtimeBackend || options.runtime_backend,
    runtime: options.runtime || options.runtimeId || options.runtime_id,
    config,
    secret_env: normalizeArray(config.secret_env),
    task_timeout_seconds: config.task_timeout_seconds || options.taskTimeoutSeconds || options.task_timeout_seconds || 900,
    limits: options.limits,
    expected_artifacts: options.expectedArtifacts || options.expected_artifacts,
  });
}

function runtimeAgentCiTaskExecutorConfig(options = {}) {
  const runtimeProfile = resolveRuntimeAgentCiRuntimeProfile(options);
  const runtimeExecution = normalizeRuntimeExecutionDescriptor(options.runtimeExecution || options.runtime_execution, runtimeProfile);
  const runtimeTaskInput = runtimeExecution?.input || options.abilityInput || options.ability_input || {};
  const runtimeTask = stripUndefined({
    ability: runtimeExecution?.ability || options.ability || runtimeProfile.runtime_task_ability,
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
    runtime_execution: runtimeExecution,
    runtime_output_projections: options.runtimeOutputProjections || options.runtime_output_projections,
    callback_data: options.callbackData || options.callback_data,
    evidence_projections: options.evidenceProjections || options.evidence_projections,
    structured_artifacts: options.structuredArtifacts || options.structured_artifacts,
    task_timeout_seconds: options.taskTimeoutSeconds || options.task_timeout_seconds,
    max_turns: options.maxTurns || options.max_turns,
    provider_plugin_paths: options.providerPluginPaths || options.provider_plugin_paths,
    secret_env: options.secretEnv || options.secret_env,
    runtime_env: options.runtimeEnv || options.runtime_env,
    runtime_config_mounts: options.runtimeConfigMounts || options.runtime_config_mounts,
    runtime_state_mounts: options.runtimeStateMounts || options.runtime_state_mounts,
    provider_runtime_invocation: options.providerRuntimeInvocation || options.provider_runtime_invocation || options.runtimeInvocation || options.runtime_invocation,
    runtime_id: options.runtimeId || options.runtime_id,
    runtime_bin: options.runtimeBin || options.runtime_bin,
  });
}

function normalizeRuntimeExecutionDescriptor(descriptor, runtimeProfile = {}) {
  if (!descriptor || typeof descriptor !== 'object' || Array.isArray(descriptor)) {
    return null;
  }
  if (Object.keys(descriptor).length === 0) {
    return null;
  }
  const kind = descriptor.kind || descriptor.type || (descriptor.workflow ? 'workflow' : descriptor.bundle || descriptor.source || descriptor.path ? 'bundle' : 'ability');
  const ability = descriptor.ability || abilityForRuntimeExecutionKind(kind, runtimeProfile);
  const input = runtimeExecutionInput(descriptor, kind);
  return stripUndefined({
    schema: descriptor.schema,
    kind,
    ability,
    input,
    outputs: descriptor.outputs,
    metadata: descriptor.metadata,
  });
}

function runtimeAgentCiBundleRuntimeExecution(options = {}, input = {}) {
  const bundle = options.bundle && typeof options.bundle === 'object' && !Array.isArray(options.bundle) ? options.bundle : {};
  const source = options.source || options.bundlePath || options.bundle_path || bundle.source || bundle.path || bundle.bundle_path || (typeof options.bundle === 'string' ? options.bundle : undefined);
  if (!source) {
    return null;
  }
  return {
    kind: 'bundle',
    ability: options.ability,
    input: stripUndefined({
      source,
      ...input,
      ...stripUndefined(options.runtimeTaskInput || options.runtime_task_input || {}),
    }),
  };
}

function runtimeAgentCiTaskFromRequest(runtimeTask = {}, abilityRequest = {}, abilityInput = {}) {
  if (runtimeTask && typeof runtimeTask === 'object' && !Array.isArray(runtimeTask) && Object.keys(runtimeTask).length > 0) {
    if (!runtimeTask.ability || typeof runtimeTask.ability !== 'string') {
      throw new Error('runtime_task.ability is required when runtime_task is supplied.');
    }
    return runtimeTask;
  }

  const hasAbilityRequest = abilityRequest && typeof abilityRequest === 'object' && !Array.isArray(abilityRequest) && Object.keys(abilityRequest).length > 0;
  const hasAbilityInput = abilityInput && typeof abilityInput === 'object' && !Array.isArray(abilityInput) && Object.keys(abilityInput).length > 0;
  if (!hasAbilityRequest && !hasAbilityInput) {
    return null;
  }
  if (!abilityRequest.ability || typeof abilityRequest.ability !== 'string') {
    throw new Error('ability_request.ability is required when ability_request or ability_input is supplied.');
  }

  return {
    ...abilityRequest,
    input: {
      ...(abilityRequest.input && typeof abilityRequest.input === 'object' && !Array.isArray(abilityRequest.input) ? abilityRequest.input : {}),
      ...abilityInput,
    },
  };
}

function runtimeAgentCiFirstNonEmptyObject(primary = {}, legacy = {}) {
  return primary && typeof primary === 'object' && !Array.isArray(primary) && Object.keys(primary).length > 0 ? primary : legacy;
}

function runtimeAgentCiFirstNonEmptyArray(primary = [], legacy = []) {
  return Array.isArray(primary) && primary.length > 0 ? primary : legacy;
}

function abilityForRuntimeExecutionKind(kind, runtimeProfile = {}) {
  if (kind === 'bundle') {
    return runtimeProfile.runtime_bundle_ability || runtimeProfile.runtime_task_ability;
  }
  if (kind === 'workflow') {
    return runtimeProfile.runtime_workflow_ability || runtimeProfile.runtime_task_ability;
  }
  return runtimeProfile.runtime_task_ability;
}

function runtimeExecutionInput(descriptor, kind) {
  const explicitInput = descriptor.input && typeof descriptor.input === 'object' && !Array.isArray(descriptor.input)
    ? descriptor.input
    : {};
  const bundle = descriptor.bundle && typeof descriptor.bundle === 'object' && !Array.isArray(descriptor.bundle)
    ? descriptor.bundle
    : {};
  const workflow = descriptor.workflow && typeof descriptor.workflow === 'object' && !Array.isArray(descriptor.workflow)
    ? descriptor.workflow
    : descriptor.workflow;
  const source = descriptor.source || descriptor.path || bundle.source || bundle.path || bundle.bundle_path;
  const normalizedBundle = Object.keys(bundle).length > 0 ? bundle : undefined;
  const normalizedWorkflow = workflow && (typeof workflow !== 'object' || Object.keys(workflow).length > 0) ? workflow : undefined;
  return stripUndefined({
    ...(kind === 'bundle' ? {
      source,
      bundle: normalizedBundle,
      workflow: normalizedWorkflow,
    } : {}),
    ...(kind === 'workflow' ? {
      source,
      path: descriptor.path,
      workflow: normalizedWorkflow,
    } : {}),
    ...explicitInput,
  });
}

function resolveRuntimeAgentCiRuntimeProfile(options = {}) {
  const runtimeProfiles = options.runtimeProfiles || options.runtime_profiles || options.config?.runtime_profiles || options.config?.runtimeProfiles || {};
  const runtimeProfilePresets = options.runtimeProfilePresets || options.runtime_profile_presets || options.config?.runtime_profile_presets || options.config?.runtimeProfilePresets || {};
  const requestedProfile = options.runtimeProfile || options.runtime_profile || options.config?.runtime_profile || options.config?.runtimeProfile || RUNTIME_AGENT_CI_RUNTIME_PROFILE_ID;
  const profile = runtimeProfiles[requestedProfile] || runtimeProfilePresets[requestedProfile] || options.runtimeProfileConfig || options.runtime_profile_config;
  if (!profile || typeof profile !== 'object' || Array.isArray(profile)) {
    throw new Error(`runtime profile ${requestedProfile} is not configured.`);
  }

  const id = requiredString(profile.id || requestedProfile, 'runtime profile id');
  return {
    ...profile,
    id,
    runtime_task_ability: runtimeProfileAbility(options.runtimeTaskAbility || options.runtime_task_ability || profile.runtime_task_ability, profile),
  };
}

function runtimeProfileAbility(value, profile) {
  const ability = value || profile.runtime_bundle_ability || profile.runtime_workflow_ability;
  return requiredString(ability, 'runtime task ability');
}

function runtimeAgentCiRuntimeProfilesForOptions(options, runtimeProfile) {
  return {
    ...(options.config?.runtime_profiles || options.config?.runtimeProfiles || {}),
    ...(options.runtimeProfiles || options.runtime_profiles || {}),
    [runtimeProfile.id]: runtimeProfile,
  };
}

const runtimeProfilesForOptions = runtimeAgentCiRuntimeProfilesForOptions;

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
  runtimeAgentCiBundleRuntimeExecution,
  runtimeAgentCiFirstNonEmptyArray,
  runtimeAgentCiFirstNonEmptyObject,
  runtimeAgentCiPlan,
  runtimeAgentCiRunnerSpec,
  runtimeAgentCiRuntimeTaskRequest,
  runtimeAgentCiRuntimeProfilesForOptions,
  runtimeAgentCiTaskFromRequest,
  runtimeAgentCiTaskExecutorConfig,
  normalizeRuntimeExecutionDescriptor,
  resolveRuntimeAgentCiRuntimeProfile,
  validateAgentTaskRunnerSpec,
};
