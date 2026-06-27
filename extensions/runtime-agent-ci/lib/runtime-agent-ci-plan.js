'use strict';

const {
  AGENT_TASK_RUNNER_SPEC_SCHEMA,
  validateAgentTaskRunnerSpec,
} = require('../../agent-task-contracts/agent-task-runner-contract');
const {
  GENERIC_AGENT_TASK_PLAN_SCHEMA,
  GENERIC_AGENT_TASK_REQUEST_SCHEMA,
  genericAgentTaskPlan,
  genericAgentTaskRequest,
  genericAgentTaskRunnerSpec,
  normalizeRuntimeExecutionDescriptor,
} = require('./generic-agent-task-plan');
const { normalizeRuntimeId, resolveRuntimeProvider, runtimeIdFromOptions } = require('./runtime-provider-resolver.cjs');
const {
  expandAgentTaskCapabilityBundles,
  expandAgentTaskToolPresets,
} = require('../../agent-task-contracts/agent-task-provider-contract');

const AGENT_TASK_PLAN_SCHEMA = GENERIC_AGENT_TASK_PLAN_SCHEMA;
const AGENT_TASK_REQUEST_SCHEMA = GENERIC_AGENT_TASK_REQUEST_SCHEMA;
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

  return genericAgentTaskRequest({
    schema: AGENT_TASK_REQUEST_SCHEMA,
    task_id: taskId,
    group_key: options.groupKey || options.group_key,
    parent_plan_id: options.parentPlanId || options.parent_plan_id,
    cwd: options.cwd,
    repo: options.repo,
    workspace: options.workspace,
    instructions: options.instructions || '',
    inputs: {
      ...(options.inputs || {}),
      ...(options.title ? { title: options.title } : {}),
    },
    runnerSpec,
  });
}

function runtimeAgentCiRunnerSpec(options = {}, context = {}) {
  const taskExecutorConfig = context.taskExecutorConfig || runtimeAgentCiTaskExecutorConfig;
  const config = taskExecutorConfig(options);
  // The executor runtime is only populated from an explicit runtime selector; a
  // bare backend (e.g. { backend: 'opencode' }) must not imply a runtime, and the
  // resolution must not fall back to DEFAULT_RUNTIME_ID here. Using the shared
  // runtimeIdFromOptions (which derives runtime from options.backend and defaults
  // to DEFAULT_RUNTIME_ID) would break that contract.
  const runtime = options.runtime || options.runtimeId || options.runtime_id || options.runtimeProvider || options.runtime_provider;
  const normalizedRuntime = runtime ? normalizeRuntimeId(runtime) : runtime;
  return genericAgentTaskRunnerSpec({
    backend: options.backend || options.runtimeBackend || options.runtime_backend || runtimeBackendForRuntime(normalizedRuntime),
    runtime: normalizedRuntime,
    config,
    secret_env: normalizeArray(config.secret_env),
    task_timeout_seconds: config.task_timeout_seconds || options.taskTimeoutSeconds || options.task_timeout_seconds || 900,
    artifact_declarations: options.artifactDeclarations || options.artifact_declarations,
    limits: options.limits,
    expected_artifacts: options.expectedArtifacts || options.expected_artifacts,
  });
}

function runtimeBackendForRuntime(runtime) {
  const normalizedRuntime = normalizeRuntimeId(runtime);
  try {
    return resolveRuntimeProvider(normalizedRuntime).executor.backend || normalizedRuntime;
  } catch {
    return normalizedRuntime;
  }
}

function runtimeAgentCiTaskExecutorConfig(options = {}) {
  const runtimeProfile = resolveRuntimeAgentCiRuntimeProfile(options);
  const runtimeId = runtimeIdFromOptions(options, {});
  const runtimeExecution = normalizeRuntimeExecutionDescriptor(options.runtimeExecution || options.runtime_execution, runtimeProfile);
  const runtimeTaskInput = runtimeExecution?.input || options.abilityInput || options.ability_input || {};
  const workload = nonEmptyObject(options.workload);
  const toolPolicy = nonEmptyObject(options.toolPolicy || options.tool_policy || options.sandboxToolPolicy || options.sandbox_tool_policy);
  const runtimeTask = stripUndefined({
    ability: runtimeExecution?.ability || options.ability || runtimeProfile.runtime_task_ability,
    input: runtimeTaskInput,
  });
  const requestedCapabilityBundles = options.capabilityBundles || options.capability_bundles || options.config?.capability_bundles || options.config?.capabilityBundles;
  const requestedProviderRuntimeInvocation = options.providerRuntimeInvocation || options.provider_runtime_invocation || options.runtimeInvocation || options.runtime_invocation || options.config?.provider_runtime_invocation || options.config?.providerRuntimeInvocation || options.config?.runtime_invocation || options.config?.runtimeInvocation;
  const capabilityExpansion = expandAgentTaskCapabilityBundles(requestedCapabilityBundles || []);
  const expandedToolPresetTools = expandAgentTaskToolPresets(capabilityExpansion.tool_presets || []);
  const providerRuntimeInvocation = mergeRuntimeInvocationDescriptors(
    capabilityExpansion.provider_runtime_invocation,
    requestedProviderRuntimeInvocation
  );
  return stripUndefined({
    ...(options.config || {}),
    provider: options.provider,
    model: options.model,
    runtime_id: runtimeId ? normalizeRuntimeId(runtimeId) : runtimeId,
    runtime_profile: runtimeProfile.id,
    runtime_profiles: runtimeProfilesForOptions(options, runtimeProfile),
    runtime_component_paths: options.runtimeComponentPaths || options.runtime_component_paths,
    component_contracts: options.componentContracts || options.component_contracts,
    homeboy_extensions: options.homeboyExtensions || options.homeboy_extensions,
    agent_bundles: options.agentBundles || options.agent_bundles,
    ignored_workspace_paths: options.ignoredWorkspacePaths || options.ignored_workspace_paths,
    runtime_task: runtimeTask,
    workload,
    sandbox_tool_policy: toolPolicy,
    capability_bundles: normalizeArray(requestedCapabilityBundles).length > 0 ? normalizeArray(requestedCapabilityBundles) : undefined,
    tool_presets: capabilityExpansion.tool_presets,
    workspace_tools: expandedToolPresetTools.workspace_tools,
    publication_tools: expandedToolPresetTools.publication_tools,
    ability_tools: options.abilityTools || options.ability_tools,
    ability_requirements: options.abilityRequirements || options.ability_requirements || runtimeProfile.ability_requirements,
    artifact_slots: options.artifactSlots || options.artifact_slots,
    artifact_declarations: options.artifactDeclarations || options.artifact_declarations,
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
    provider_runtime_invocation: nonEmptyObject(providerRuntimeInvocation),
    runtime_bin: options.runtimeBin || options.runtime_bin,
  });
}

function mergeRuntimeInvocationDescriptors(primary = {}, secondary = {}) {
  const merged = {};
  for (const descriptor of [primary, secondary]) {
    if (!descriptor || typeof descriptor !== 'object' || Array.isArray(descriptor)) {
      continue;
    }
    const existingOperations = merged.operations;
    Object.assign(merged, descriptor);
    const operations = mergeRuntimeInvocationOperations(existingOperations, runtimeInvocationOperations(descriptor));
    if (Object.keys(operations).length > 0) {
      merged.operations = operations;
    }
  }
  return merged;
}

function mergeRuntimeInvocationOperations(current = {}, next = {}) {
  const merged = current && typeof current === 'object' && !Array.isArray(current) ? { ...current } : {};
  for (const [operation, config] of Object.entries(next && typeof next === 'object' && !Array.isArray(next) ? next : {})) {
    if (config && typeof config === 'object' && !Array.isArray(config) && merged[operation] && typeof merged[operation] === 'object' && !Array.isArray(merged[operation])) {
      merged[operation] = { ...merged[operation], ...config };
    } else {
      merged[operation] = config;
    }
  }
  return merged;
}

function runtimeInvocationOperations(value = {}) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return {};
  }
  const operations = value.operations || value.provider_operations || value.providerOperations || value.tasks || value.abilities;
  return operations && typeof operations === 'object' && !Array.isArray(operations) ? operations : {};
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
  return genericAgentTaskPlan({
    schema: AGENT_TASK_PLAN_SCHEMA,
    plan_id: options.planId || options.plan_id,
    tasks: options.tasks,
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

function nonEmptyObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length > 0 ? value : undefined;
}

module.exports = {
  AGENT_TASK_PLAN_SCHEMA,
  AGENT_TASK_REQUEST_SCHEMA,
  AGENT_TASK_RUNNER_SPEC_SCHEMA,
  RUNTIME_AGENT_CI_RUNTIME_PROFILE_ID,
  genericAgentTaskPlan,
  genericAgentTaskRequest,
  genericAgentTaskRunnerSpec,
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
  runtimeBackendForRuntime,
  normalizeRuntimeExecutionDescriptor,
  resolveRuntimeAgentCiRuntimeProfile,
  validateAgentTaskRunnerSpec,
};
