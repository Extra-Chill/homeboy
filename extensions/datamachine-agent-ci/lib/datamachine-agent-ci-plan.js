'use strict';

const {
  AGENT_TASK_RUNNER_SPEC_SCHEMA,
  AGENT_TASK_PLAN_SCHEMA,
  AGENT_TASK_REQUEST_SCHEMA,
  agentTaskRequestFromRunnerSpec,
  runtimeAgentCiAbilityTaskRequest,
  runtimeAgentCiBundleRuntimeExecution,
  runtimeAgentCiPlan,
  runtimeAgentCiRuntimeProfilesForOptions,
  runtimeAgentCiRunnerSpec,
  runtimeAgentCiTaskExecutorConfig,
  resolveRuntimeAgentCiRuntimeProfile,
  validateAgentTaskRunnerSpec,
} = require('../../runtime-agent-ci');

const DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY = 'datamachine/run-agent-bundle';
const DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID = 'datamachine-agent-ci';
const DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_PRESET = 'datamachine';

const DATAMACHINE_AGENT_CI_COMPONENT_PATH_DEFAULTS = {
  contract_slug_map: {
    'agents-api': 'agents_api',
    'data-machine': 'agent_runtime',
    'data-machine-code': 'agent_runtime_tools',
  },
  path_aliases: {
    agents_api: [
      'explicit:agents_api',
      'runtime_component:agents_api',
      'contract:agents_api',
      'config:agents_api',
      'config_path:agents_api',
      'option:agentsApi',
    ],
    agent_runtime: [
      'explicit:agent_runtime',
      'runtime_component:agent_runtime',
      'runtime_component:data_machine',
      'contract:agent_runtime',
      'config:agent_runtime',
      'config_path:agent_runtime',
      'config:data_machine',
      'config_path:data_machine',
      'option:agentRuntime',
      'option:legacyRuntime',
    ],
    agent_runtime_tools: [
      'explicit:agent_runtime_tools',
      'runtime_component:agent_runtime_tools',
      'runtime_component:data_machine_code',
      'contract:agent_runtime_tools',
      'config:agent_runtime_tools',
      'config_path:agent_runtime_tools',
      'config:data_machine_code',
      'config_path:data_machine_code',
      'option:agentRuntimeTools',
      'option:legacyRuntimeTools',
    ],
  },
  discovery: {
    agents_api: [
      { settings: ['wp_codebox_agents_api_path', 'agents_api_path'] },
      { env: 'HOMEBOY_WP_CODEBOX_AGENTS_API_PATH' },
      {
        bundled_provider: 'agent_runtime',
        paths: [
          'vendor/wordpress/agents-api',
          'vendor/automattic/agents-api',
        ],
      },
    ],
    agent_runtime: [
      { settings: ['wp_codebox_data_machine_path', 'data_machine_path'] },
      { env: 'HOMEBOY_DATA_MACHINE_PATH' },
      { active_plugin: 'data-machine' },
      { sibling: 'data-machine' },
    ],
    agent_runtime_tools: [
      { settings: ['wp_codebox_data_machine_code_path', 'data_machine_code_path'] },
      { env: 'HOMEBOY_DATA_MACHINE_CODE_PATH' },
      { active_plugin: 'data-machine-code' },
      { sibling: 'data-machine-code' },
    ],
  },
};

const DATAMACHINE_AGENT_CI_WORKSPACE_TOOLS = {
  readonly: [
    'workspace_ls',
    'workspace_read',
    'workspace_git_status',
  ],
  readwrite: [
    'workspace_run_runner_command',
    'workspace_write',
    'workspace_edit',
    'workspace_apply_patch',
    'workspace_delete',
    'workspace_git_add',
  ],
};

const DATAMACHINE_AGENT_CI_CAPABILITIES = [
  `tool:${DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY}`,
  'tool:github_issue_publish',
  'tool:github_pull_request_publish',
  'tool:comment_github_pull_request',
  `ability:${DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY}`,
  'ability:github_issue_publish',
  'ability:github_pull_request_publish',
  'ability:comment_github_pull_request',
];

const DATAMACHINE_AGENT_CI_ABILITY_REQUIREMENTS = [
  DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY,
  'github_issue_publish',
  'github_pull_request_publish',
  'comment_github_pull_request',
];

const DATAMACHINE_AGENT_CI_RUNTIME_ENV_ALIASES = {
  HOMEBOY_AGENT_TOOL_POLICY_JSON: ['DATAMACHINE_HOST_TOOL_POLICY_JSON'],
};

const DATAMACHINE_AGENT_CI_RUNTIME_PROFILE = {
  schema: 'homeboy/runtime-profile/v1',
  id: DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID,
  preset: DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_PRESET,
  runtime_task_ability: DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY,
  component_path_defaults: DATAMACHINE_AGENT_CI_COMPONENT_PATH_DEFAULTS,
  workspace_tools: DATAMACHINE_AGENT_CI_WORKSPACE_TOOLS,
  capabilities: DATAMACHINE_AGENT_CI_CAPABILITIES,
  ability_requirements: DATAMACHINE_AGENT_CI_ABILITY_REQUIREMENTS,
  runtime_env_aliases: DATAMACHINE_AGENT_CI_RUNTIME_ENV_ALIASES,
};

const DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_PRESETS = {
  [DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_PRESET]: DATAMACHINE_AGENT_CI_RUNTIME_PROFILE,
  [DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID]: DATAMACHINE_AGENT_CI_RUNTIME_PROFILE,
};

function datamachineAgentCiBundleTaskRequest(options = {}, context = {}) {
  const taskId = requiredString(options.taskId || options.task_id, 'taskId');
  const source = requiredString(options.source || options.bundle, 'source');
  const agentSlug = requiredString(options.agentSlug || options.agent_slug, 'agentSlug');
  const pipelineSlug = requiredString(options.pipelineSlug || options.pipeline_slug, 'pipelineSlug');
  const flowSlug = requiredString(options.flowSlug || options.flow_slug, 'flowSlug');

  const runtimeTaskInput = datamachineAgentCiBundleInput({
    ...options,
    source,
    agentSlug,
    pipelineSlug,
    flowSlug,
  });

  return datamachineAgentCiAbilityTaskRequest({
    ...options,
    taskId,
    ability: options.ability,
    abilityInput: runtimeTaskInput,
    runtimeExecution: options.runtimeExecution || options.runtime_execution || runtimeAgentCiBundleRuntimeExecution(options, runtimeTaskInput),
  }, context);
}

function datamachineAgentCiAbilityTaskRequest(options = {}, context = {}) {
  const runtimeOptions = datamachineAgentCiRuntimeOptions(options);
  return runtimeAgentCiAbilityTaskRequest(runtimeOptions, context);
}

function datamachineAgentCiRunnerSpec(options = {}, context = {}) {
  const taskExecutorConfig = context.taskExecutorConfig || datamachineAgentCiTaskExecutorConfig;
  return runtimeAgentCiRunnerSpec(options, { taskExecutorConfig });
}

function datamachineAgentCiTaskExecutorConfig(options = {}) {
  return runtimeAgentCiTaskExecutorConfig(datamachineAgentCiRuntimeOptions(options));
}

function datamachineAgentCiRuntimeOptions(options = {}) {
  const runtimeProfile = resolveRuntimeAgentCiRuntimeProfile({
    ...options,
    runtimeProfile: options.runtimeProfile || options.runtime_profile || options.config?.runtime_profile || options.config?.runtimeProfile || DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID,
    runtimeProfilePresets: {
      ...DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_PRESETS,
      ...(options.config?.runtime_profile_presets || options.config?.runtimeProfilePresets || {}),
      ...(options.runtimeProfilePresets || options.runtime_profile_presets || {}),
    },
  });

  return {
    ...options,
    ability: options.ability || runtimeProfile.runtime_task_ability,
    runtimeExecution: options.runtimeExecution || options.runtime_execution || datamachineAgentCiRuntimeExecution(options),
    runtimeProfile: runtimeProfile.id,
    runtimeProfiles: runtimeAgentCiRuntimeProfilesForOptions(options, runtimeProfile),
  };
}

function datamachineAgentCiRuntimeExecution(options = {}) {
  return runtimeAgentCiBundleRuntimeExecution(options, datamachineAgentCiBundleInput(options));
}

function datamachineAgentCiBundleInput(options = {}) {
  return stripUndefined({
    source: options.source || options.bundle,
    agent_slug: options.agentSlug || options.agent_slug,
    pipeline_slug: options.pipelineSlug || options.pipeline_slug,
    flow_slug: options.flowSlug || options.flow_slug,
    target_repo: options.targetRepo || options.target_repo,
    prompt: options.prompt || '',
    wait_for_completion: options.waitForCompletion ?? options.wait_for_completion ?? true,
    success_requires_pr: options.successRequiresPr ?? options.success_requires_pr,
    success_completion_outcomes: options.successCompletionOutcomes || options.success_completion_outcomes,
    artifact_outputs: options.artifactOutputs || options.artifact_outputs,
    flow_step_patches: options.flowStepPatches || options.flow_step_patches,
    evidence_projections: options.evidenceProjections || options.evidence_projections,
    tool_recorders: options.toolRecorders || options.tool_recorders,
    runtime_output_projections: options.runtimeOutputProjections || options.runtime_output_projections,
    engine_data_outputs: options.engineDataOutputs || options.engine_data_outputs,
    transcript_artifact_name: options.transcriptArtifactName || options.transcript_artifact_name,
    artifacts: options.artifactsPath || options.artifacts,
    max_turns: options.maxTurns || options.max_turns,
    step_budget: options.stepBudget || options.step_budget,
    time_budget_ms: options.timeBudgetMs || options.time_budget_ms,
    complexity_policy: options.complexityPolicy || options.complexity_policy,
    ...stripUndefined(options.runtimeTaskInput || options.runtime_task_input || {}),
  });
}

function datamachineAgentCiPlan(options = {}) {
  return runtimeAgentCiPlan(options);
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
  DATAMACHINE_AGENT_CI_CAPABILITIES,
  DATAMACHINE_AGENT_CI_ABILITY_REQUIREMENTS,
  DATAMACHINE_AGENT_CI_COMPONENT_PATH_DEFAULTS,
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE,
  DATAMACHINE_AGENT_CI_RUNTIME_ENV_ALIASES,
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID,
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_PRESET,
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_PRESETS,
  DATAMACHINE_AGENT_CI_WORKSPACE_TOOLS,
  DATAMACHINE_RUN_AGENT_BUNDLE_ABILITY,
  agentTaskRequestFromRunnerSpec,
  datamachineAgentCiAbilityTaskRequest,
  datamachineAgentCiBundleTaskRequest,
  datamachineAgentCiPlan,
  datamachineAgentCiRunnerSpec,
  datamachineAgentCiTaskExecutorConfig,
  validateAgentTaskRunnerSpec,
};
