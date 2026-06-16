'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

const AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const AGENT_TASK_OUTCOME_SCHEMA = 'homeboy/agent-task-outcome/v1';
const AGENT_TASK_ARTIFACT_SCHEMA = 'homeboy/agent-task-artifact/v1';
const WP_CODEBOX_TASK_REQUEST_SCHEMA = 'wp-codebox/task-input/v1';
const HOMEBOY_WORDPRESS_BACKEND = 'wordpress';
const LEGACY_CODEBOX_BACKEND = 'codebox';

const PROVIDER_CAPABILITIES = [
  'browser_runtime',
  'wordpress_sandbox',
  'workspace_mounts',
  'workspace_tools',
  'artifact_materialization',
  'patch_artifacts',
  'verification_artifacts',
  'run_registry',
  'cleanup_observability',
  'screenshots',
  'structured_outcome',
  'agent_bundle_execution',
  'typed_bundle_outputs',
  'external_recipe_packs',
  'recipe_probe_artifacts',
  'tool:datamachine/run-agent-bundle',
  'tool:github_issue_publish',
  'tool:github_pull_request_publish',
  'tool:comment_github_pull_request',
  'tool:wpsg_materialize_packet',
  'ability:datamachine/run-agent-bundle',
  'ability:github_issue_publish',
  'ability:github_pull_request_publish',
  'ability:comment_github_pull_request',
  'ability:wpsg_materialize_packet',
];

const DEFAULT_WORKSPACE_READONLY_TOOLS = [
  'workspace_ls',
  'workspace_read',
  'workspace_git_status',
];

// Additional workspace tools exposed when the repo workspace is mounted
// read-write, so a coding task can actually edit files instead of only
// inspecting them. These ids are registered by the data-machine-code runtime.
const DEFAULT_WORKSPACE_WRITE_TOOLS = [
  'workspace_run_runner_command',
  'workspace_write',
  'workspace_edit',
  'workspace_apply_patch',
  'workspace_delete',
  'workspace_git_add',
];

const CODEX_SECRET_ENV = [
  'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
  'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
  'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
];

const CLAUDE_CODE_SECRET_ENV = [
  'AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN',
];

const DEFAULT_CODEX_MODEL = 'gpt-5.5';
const CODEX_PROVIDER_PLUGIN_GUIDANCE = `Codex tasks require a Codex-capable provider plugin checkout, such as the Codex PR branch for ai-provider-for-openai. Released ai-provider-for-openai trunk registers openai, not codex, and unrelated provider defaults such as ${['ai-provider-for', ['open', 'code'].join('')].join('-')} will not work.`;
const CODEX_PHP_AI_CLIENT_GUIDANCE = 'Codex tasks require a php-ai-client checkout that supports RequestAuthenticationMethod::bearerToken and has Composer vendor dependencies installed before WP Codebox prepares the runtime overlay.';
const RUNTIME_OVERLAY_CANONICAL_SHAPE = 'runtime_overlays entries must be objects with a non-empty string kind, for example { "kind": "bundled-library", "library": "php-ai-client", "source": "/path/to/php-ai-client" }. The legacy type field is not accepted.';

const AGENT_BUNDLE_CONFIG_FIELDS = [
  'bundle_path',
  'bundle_host_path',
  'bundle_repo',
  'bundle_ref',
  'bundle_path_in_repo',
  'agent_slug',
  'pipeline_slug',
  'flow_slug',
  'target_repo',
  'component_path',
  'component_id',
  'provider',
  'model',
  'prompt',
  'success_requires_pr',
  'success_completion_outcomes',
  'pipeline_step_patches',
  'flow_step_patches',
  'tool_recorders',
  'ability_tools',
  'engine_data_outputs',
  'engine_key',
  'tool_results_key',
  'artifact_export_config',
  'transcript_artifact_name',
  'replay_bundle_artifact_name',
  'replay_bundle_dir',
  'runner_workspace',
  'extra_required_abilities',
  'enable_terminal_actions',
  'enable_wp_cli_tool',
  'wp_cli_tool_name',
  'workload_run_before',
  'workload_run_after',
  'wp_codebox_mounts',
  'extra_wp_config_defines',
  'provider_plugin',
  'provider_plugin_paths',
  'step_budget',
  'time_budget_ms',
];

const AGENT_BUNDLE_TRIGGER_FIELDS = AGENT_BUNDLE_CONFIG_FIELDS.filter((field) => ![
  'provider',
  'model',
  'prompt',
  'provider_plugin_paths',
].includes(field));

const LEGACY_RUNTIME_PREFIX = ['data', 'machine'].join('_');
const LEGACY_BUNDLE_KEYS = [
  `${LEGACY_RUNTIME_PREFIX}_bundle`,
  `${LEGACY_RUNTIME_PREFIX}Bundle`,
];

const WP_CODEBOX_RUNTIME_GAP_TRACKERS = [];

const WP_CODEBOX_ROLE_ALIASES = {
  artifact_kinds: {
    patch: ['codebox-patch'],
  },
  artifact_filenames: {
    preflight_evidence: ['homeboy-codebox-task-runner.json'],
  },
  outputs: {
    provider_run_result: ['codebox_run_result'],
  },
  metadata: {
    provider_run_result: ['codebox_run_result'],
  },
};

const AGENT_TASK_OUTCOME_STATUSES = [
  'succeeded',
  'failed',
  'no_op',
  'unable_to_remediate',
  'timeout',
  'provider_error',
];

const AGENT_TASK_FAILURE_CLASSIFICATIONS = [
  'provider',
  'timeout',
  'execution_failed',
];

const AGENT_TASK_REDACTED_METADATA_KEYS = [
  'secret_env_values',
  'secretEnvValues',
  'secrets',
];

function assertAgentTaskRequest(request) {
  if (!request || request.schema !== AGENT_TASK_REQUEST_SCHEMA) {
    throw new Error(`Agent task request must use schema ${AGENT_TASK_REQUEST_SCHEMA}.`);
  }
  if (!request.task_id) {
    throw new Error('Agent task request requires task_id.');
  }
  const backend = request.executor?.backend;
  if (![HOMEBOY_WORDPRESS_BACKEND, LEGACY_CODEBOX_BACKEND].includes(backend)) {
    throw new Error('WordPress executor provider only accepts executor.backend "wordpress".');
  }
}

function providerContract(options = {}) {
  return {
    schema: 'homeboy/agent-task-executor-provider/v1',
    id: options.id || 'wordpress.agent-task-executor',
    label: options.label || 'WordPress agent task executor',
    backend: HOMEBOY_WORDPRESS_BACKEND,
    command: options.command || 'node {{extension_path}}/scripts/agent/homeboy-codebox-agent-task-executor.cjs',
    request_schema: AGENT_TASK_REQUEST_SCHEMA,
    outcome_schema: AGENT_TASK_OUTCOME_SCHEMA,
    request_required_fields: ['schema', 'task_id', 'executor.backend', 'instructions'],
    outcome_statuses: AGENT_TASK_OUTCOME_STATUSES,
    failure_classifications: AGENT_TASK_FAILURE_CLASSIFICATIONS,
    redacted_metadata_keys: AGENT_TASK_REDACTED_METADATA_KEYS,
    capabilities: PROVIDER_CAPABILITIES,
    workspace_materialization: {
      cwd: 'git_checkout',
    },
    role_aliases: WP_CODEBOX_ROLE_ALIASES,
    status: 'active',
    integration_contract: 'homeboy-wordpress-agent-task/v1',
    runtime_gap_trackers: WP_CODEBOX_RUNTIME_GAP_TRACKERS,
  };
}

function codeboxTaskRequestFromAgentTaskRequest(request, options = {}) {
  assertAgentTaskRequest(request);
  const config = request.executor.config || {};
  const inputs = request.inputs || {};
  const defaults = defaultCodeboxRuntimeConfig(request, config, inputs, options);
  const agentBundle = agentBundleConfigFromAgentTaskRequest(request, config, inputs);
  const recipe = recipeConfigFromAgentTaskRequest(request, config, inputs);
  const mounts = agentBundleMounts(agentBundle, config.mounts || defaults.mounts || options.mounts || []);
  const componentContracts = componentContractsFromAgentTaskRequest(request, config, options);
  const components = runtimeComponentPaths(config, { ...defaults, ...options, componentContracts });
  const agentBundles = firstDefined(inputs.agent_bundles, inputs.agentBundles, config.agent_bundles, config.agentBundles, options.agentBundles, []);
  const structuredArtifacts = firstDefined(inputs.structured_artifacts, inputs.structuredArtifacts, config.structured_artifacts, config.structuredArtifacts, options.structuredArtifacts, []);
  const artifactDeclarations = artifactDeclarationsFromAgentTaskRequest(request, config, inputs, options);
  const allowedTools = allowedToolsFromAgentTaskRequest(request, config, inputs, options, defaults);
  const sandboxToolPolicy = sandboxToolPolicyFromAgentTaskRequest(config, inputs, options, defaults, allowedTools);
  const provider = config.provider || options.provider || defaults.provider || '';
  const model = request.executor.model || config.model || options.model || defaults.model || '';
  const runtimeTask = runtimeTaskWithExecutionDefaults(
    inputs.runtime_task || inputs.runtimeTask || config.runtime_task || config.runtimeTask || abilityRuntimeTaskFromAgentTaskRequest(config, inputs) || options.runtimeTask,
    { provider, model, agentBundles }
  );
  const explicitSecretEnv = [
    ...normalizeArray(request.executor?.secret_env),
    ...normalizeArray(config.secret_env),
    ...normalizeArray(options.secretEnv),
  ];
  const timeoutSeconds = request.limits?.task_timeout_seconds || request.limits?.taskTimeoutSeconds;
  const timeoutMs = request.limits?.timeout_ms || request.limits?.max_runtime_ms;
  const timeoutFromMs = timeoutMs ? Math.ceil(timeoutMs / 1000) : undefined;
  const runtimeOverlays = runtimeOverlaysFromConfig(config, options, defaults);
  const context = {
    ...(inputs.context || {}),
    agent_task_id: request.task_id,
    group_key: request.group_key,
    parent_plan_id: request.parent_plan_id,
    source_refs: request.source_refs || [],
    audit_findings: inputs.audit_findings || [],
    matrix: inputs.matrix,
  };

  return {
    schema: WP_CODEBOX_TASK_REQUEST_SCHEMA,
    goal: request.instructions,
    target: inputs.target || request.workspace || {},
    allowed_tools: allowedTools || [],
    expected_artifacts: request.expected_artifacts || [],
    artifact_declarations: artifactDeclarations,
    policy: request.policy || {},
    context,
    recipe,
    sandbox_tool_policy: sandboxToolPolicy,
    runtime_task: runtimeTask,
    ability_tools: firstDefined(inputs.ability_tools, inputs.abilityTools, config.ability_tools, config.abilityTools, options.abilityTools, []),
    structured_artifacts: structuredArtifacts,
    sandbox_session_id: config.sandbox_session_id || request.task_id,
    session_id: config.session_id || config.sessionId || '',
    agent: config.agent || options.agent || 'wp-codebox-sandbox',
    mode: config.mode || options.mode || 'sandbox',
    provider,
    model,
    provider_plugin_paths: config.provider_plugin_paths || options.providerPluginPaths || defaults.providerPluginPaths || [],
    agent_bundles: agentBundles,
    runtime_stack_mounts: config.runtime_stack_mounts || options.runtimeStackMounts || [],
    runtime_overlay_profiles: config.runtime_overlay_profiles || config.runtimeOverlayProfiles || options.runtimeOverlayProfiles || defaults.runtimeOverlayProfiles || [],
    runtime_overlays: runtimeOverlays,
    runtime_env: firstDefined(config.runtime_env, config.runtimeEnv, config.wp_codebox_runtime_env, options.runtimeEnv, defaults.runtimeEnv, {}),
    runtime_state_mounts: firstDefined(config.runtime_state_mounts, config.runtimeStateMounts, config.wp_codebox_runtime_state_mounts, options.runtimeStateMounts, defaults.runtimeStateMounts, []),
    runtime_config_mounts: firstDefined(config.runtime_config_mounts, config.runtimeConfigMounts, config.wp_codebox_runtime_config_mounts, options.runtimeConfigMounts, defaults.runtimeConfigMounts, []),
    secret_env: explicitSecretEnv.length > 0 ? Array.from(new Set(explicitSecretEnv)) : defaults.secretEnv || [],
    // Post-agent verification gate (recipe workflow.after). Supplied as WP
    // Codebox recipe steps; a non-zero exit fails the run so the orchestrator
    // refuses to report success until the gates are green.
    verify_steps: inputs.verify_steps || config.verify_steps || options.verifySteps || [],
    mounts,
    workspaces: inputs.workspaces || config.workspaces || options.workspaces || defaults.workspaces || [],
    runtime_component_paths: components,
    component_contracts: componentContracts,
    homeboy_path: config.homeboy || config.homeboy_path || options.homeboy || '',
    homeboy_extensions_path: config.homeboy_extensions || config.homeboy_extensions_path || options.homeboyExtensions || '',
    wp_codebox_bin: firstValue(config.wp_codebox_bin, config.wpCodeboxBin, options.wpCodeboxBin, defaults.wpCodeboxBin, ''),
    wp: config.wp_codebox_wordpress_version || config.wpCodeboxWordpressVersion || config.wp || config.wordpress_version || options.wpCodeboxWordpressVersion || '',
    artifacts_path: config.artifacts || config.artifacts_path || options.artifacts || '',
    max_turns: config.max_turns || options.maxTurns,
    task_timeout_seconds: config.task_timeout_seconds || timeoutSeconds || timeoutFromMs || options.taskTimeoutSeconds,
    orchestrator: {
      ...(inputs.orchestrator || {}),
      agent_task_id: request.task_id,
      parent_plan_id: request.parent_plan_id,
      source_refs: request.source_refs || [],
    },
    agent_bundle: agentBundle,
    parent_request: request,
  };
}

function artifactDeclarationsFromAgentTaskRequest(request, config = {}, inputs = {}, options = {}) {
  const declarations = firstDefined(
    request.artifact_declarations,
    request.artifactDeclarations,
    inputs.artifact_declarations,
    inputs.artifactDeclarations,
    config.artifact_declarations,
    config.artifactDeclarations,
    options.artifactDeclarations,
    []
  );
  return Array.isArray(declarations) ? declarations : [];
}

class RuntimeOverlayConfigError extends Error {
  constructor(diagnostics) {
    super(diagnostics[0]?.message || 'Invalid WordPress executor runtime overlay config.');
    this.name = 'RuntimeOverlayConfigError';
    this.diagnostics = diagnostics;
  }
}

function runtimeOverlaysFromConfig(config, options = {}, defaults = {}) {
  return validateRuntimeOverlays(firstDefined(
    config.runtime_overlays,
    options.runtimeOverlays,
    defaults.runtimeOverlays,
    []
  ));
}

function validateRuntimeOverlays(value) {
  const overlays = normalizeArray(value);
  const diagnostics = [];

  overlays.forEach((overlay, index) => {
    const pathPrefix = `runtime_overlays[${index}]`;
    if (!overlay || typeof overlay !== 'object' || Array.isArray(overlay)) {
      diagnostics.push(runtimeOverlayDiagnostic(index, pathPrefix, 'entry', overlay, 'Runtime overlay entries must be objects.'));
      return;
    }

    if (Object.hasOwn(overlay, 'type')) {
      diagnostics.push(runtimeOverlayDiagnostic(index, `${pathPrefix}.type`, 'type', overlay.type, 'Runtime overlay config uses legacy field "type"; use canonical field "kind" instead.'));
    }

    if (typeof overlay.kind !== 'string' || overlay.kind.trim() === '') {
      diagnostics.push(runtimeOverlayDiagnostic(index, `${pathPrefix}.kind`, 'kind', overlay.kind, 'Runtime overlay config requires canonical field "kind" as a non-empty string.'));
    }
  });

  if (diagnostics.length > 0) {
    throw new RuntimeOverlayConfigError(diagnostics);
  }

  return overlays;
}

function runtimeOverlayDiagnostic(index, field, offendingField, value, message) {
  return {
    class: 'codebox.runtime_overlay_config_invalid',
    message: `Invalid WordPress executor runtime overlay config at runtime_overlays[${index}]: ${message} ${RUNTIME_OVERLAY_CANONICAL_SHAPE}`,
    data: {
      overlay_index: index,
      field,
      offending_field: offendingField,
      offending_value: value,
      expected: RUNTIME_OVERLAY_CANONICAL_SHAPE,
    },
  };
}

function componentContractsFromAgentTaskRequest(request, config, options = {}) {
  return uniqueComponentContracts([
    ...normalizeArray(request.component_contracts),
    ...normalizeArray(config.component_contracts),
    ...normalizeArray(options.componentContracts),
  ]);
}

function uniqueComponentContracts(contracts) {
  const seen = new Set();
  return contracts.filter((contract) => {
    if (!contract || typeof contract !== 'object' || Array.isArray(contract)) {
      return false;
    }
    const key = `${contract.slug || ''}:${contract.path || contract.source || ''}`;
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
}

function abilityRuntimeTaskFromAgentTaskRequest(config, inputs) {
  const genericAbilityTask = genericAbilityRuntimeTask(config, inputs);
  if (genericAbilityTask) {
    return genericAbilityTask;
  }

  const executionKind = firstValue(inputs.execution_kind, inputs.executionKind, config.execution_kind, config.executionKind);
  if (!['wp_codebox_ability', 'wordpress_ability'].includes(executionKind)) {
    return null;
  }
  const ability = firstValue(inputs.ability, inputs.ability_name, inputs.abilityName, config.ability, config.ability_name, config.abilityName);
  if (!ability || typeof ability !== 'string') {
    return null;
  }
  const input = firstObject(inputs.ability_input, inputs.abilityInput, inputs.input, config.ability_input, config.abilityInput, config.input) || {};
  return { ability, input };
}

function genericAbilityRuntimeTask(config, inputs) {
  const rawAbility = firstValue(inputs.ability, config.ability);
  const abilityRequest = firstObject(
    inputs.ability_request,
    inputs.abilityRequest,
    config.ability_request,
    config.abilityRequest,
  );
  const declared = abilityRequest || firstObject(rawAbility);
  const ability = typeof declared === 'string'
    ? declared
    : firstValue(declared?.id, declared?.name, declared?.ability, typeof rawAbility === 'string' ? rawAbility : '', inputs.ability_name, inputs.abilityName, config.ability_name, config.abilityName);
  if (!ability || typeof ability !== 'string') {
    return null;
  }
  const input = firstObject(declared?.input, declared?.args, inputs.ability_input, inputs.abilityInput, inputs.input, config.ability_input, config.abilityInput, config.input) || {};
  return { ability, input };
}

function allowedToolsFromAgentTaskRequest(request, config, inputs, options, defaults) {
  const explicit = firstDefined(inputs.allowed_tools, inputs.allowedTools, config.allowed_tools, config.allowedTools, options.allowedTools);
  if (explicit !== undefined) {
    return normalizeToolIds(explicit);
  }

  const declared = normalizeToolIds([
    ...normalizeArray(request.tools),
    ...normalizeArray(request.tool_requirements),
    ...normalizeArray(request.toolRequirements),
    ...normalizeArray(request.abilities),
    ...normalizeArray(request.ability_requirements),
    ...normalizeArray(request.abilityRequirements),
    ...normalizeArray(inputs.tools),
    ...normalizeArray(inputs.tool_requirements),
    ...normalizeArray(inputs.toolRequirements),
    ...normalizeArray(inputs.abilities),
    ...normalizeArray(inputs.ability_requirements),
    ...normalizeArray(inputs.abilityRequirements),
    ...normalizeArray(config.tools),
    ...normalizeArray(config.tool_requirements),
    ...normalizeArray(config.toolRequirements),
    ...normalizeArray(config.abilities),
    ...normalizeArray(config.ability_requirements),
    ...normalizeArray(config.abilityRequirements),
  ]);
  return uniqueStrings([...(defaults.allowedTools || []), ...declared]);
}

function sandboxToolPolicyFromAgentTaskRequest(config, inputs, options, defaults, allowedTools) {
  const explicit = firstDefined(inputs.sandbox_tool_policy, inputs.sandboxToolPolicy, config.sandbox_tool_policy, config.sandboxToolPolicy, options.sandboxToolPolicy);
  if (explicit !== undefined) {
    return explicit;
  }
  return workspaceSandboxToolPolicyWithAllowedTools(defaults.sandboxToolPolicy, allowedTools);
}

function normalizeToolIds(value) {
  return uniqueStrings(normalizeArray(value).flatMap((entry) => {
    if (typeof entry === 'string') {
      return [entry];
    }
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      return [];
    }
    return [entry.id, entry.name, entry.tool, entry.ability, entry.capability].filter(Boolean);
  }));
}

function uniqueStrings(values) {
  const seen = new Set();
  return values.filter((value) => {
    const normalized = typeof value === 'string' ? value.trim() : '';
    if (!normalized || seen.has(normalized)) {
      return false;
    }
    seen.add(normalized);
    return true;
  });
}

function workspaceSandboxToolPolicyWithAllowedTools(basePolicy, allowedTools) {
  if (!basePolicy || typeof basePolicy !== 'object' || !Array.isArray(allowedTools)) {
    return basePolicy;
  }
  const existingRuntimeToolIds = new Set((basePolicy.tools || []).map((tool) => tool?.runtime_tool_id || tool?.id).filter(Boolean));
  const extraTools = allowedTools
    .filter((tool) => !existingRuntimeToolIds.has(tool))
    .map((tool) => ({
      id: tool,
      runtime_tool_id: tool,
      execution_location: 'sandbox',
      transport_visibility: 'sandbox',
      allowed: true,
      runtime: {
        environment: 'runtime_local',
        capability_scope: 'runtime_local',
      },
      metadata: { source: 'homeboy.wordpress-agent-task.generic-tool-requirement' },
    }));
  if (extraTools.length === 0) {
    return basePolicy;
  }
  return {
    ...basePolicy,
    tools: [...(basePolicy.tools || []), ...extraTools],
  };
}

function runtimeTaskWithExecutionDefaults(runtimeTask, defaults = {}) {
  if (!runtimeTask || typeof runtimeTask !== 'object' || Array.isArray(runtimeTask)) {
    return runtimeTask;
  }
  if (!Array.isArray(defaults.agentBundles) || defaults.agentBundles.length === 0) {
    return runtimeTask;
  }

  const input = runtimeTask.input && typeof runtimeTask.input === 'object' && !Array.isArray(runtimeTask.input)
    ? runtimeTask.input
    : {};
  const defaultInput = Object.fromEntries(Object.entries({
    provider: defaults.provider,
    model: defaults.model,
  }).filter(([, value]) => value !== '' && value !== undefined));

  if (Object.keys(defaultInput).length === 0) {
    return runtimeTask;
  }

  return {
    ...runtimeTask,
    input: {
      ...defaultInput,
      ...input,
    },
  };
}

function runtimeComponentPaths(config, options = {}) {
  const explicit = config.runtime_component_paths && typeof config.runtime_component_paths === 'object'
    ? config.runtime_component_paths
    : {};
  const contractPaths = runtimeComponentPathsFromContracts(config.component_contracts || options.componentContracts || []);
  const legacyRuntimePath = config[LEGACY_RUNTIME_PREFIX] || config[`${LEGACY_RUNTIME_PREFIX}_path`] || options.legacyRuntime;
  const legacyToolsKey = `${LEGACY_RUNTIME_PREFIX}_code`;
  const legacyRuntimeToolsPath = config[legacyToolsKey] || config[`${legacyToolsKey}_path`] || options.legacyRuntimeTools;
  return Object.fromEntries(Object.entries({
    ...contractPaths,
    ...explicit,
    agents_api: explicit.agents_api || contractPaths.agents_api || config.agents_api || config.agents_api_path || options.agentsApi,
    agent_runtime: explicit.agent_runtime || contractPaths.agent_runtime || config.agent_runtime || config.agent_runtime_path || legacyRuntimePath,
    agent_runtime_tools: explicit.agent_runtime_tools || contractPaths.agent_runtime_tools || config.agent_runtime_tools_path || config.agent_runtime_tools || legacyRuntimeToolsPath,
  }).filter(([, value]) => value !== undefined && value !== ''));
}

function runtimeComponentPathsFromContracts(contracts) {
  if (!Array.isArray(contracts)) {
    return {};
  }
  const slugToKey = new Map([
    ['agents-api', 'agents_api'],
    ['data-machine', 'agent_runtime'],
    ['data-machine-code', 'agent_runtime_tools'],
  ]);
  return Object.fromEntries(contracts
    .map((contract) => [slugToKey.get(contract?.slug), contract?.path || contract?.source])
    .filter(([key, value]) => key && value));
}

function defaultCodeboxRuntimeConfig(request, config, inputs, options = {}) {
  const settings = firstObject(options.settings, parseJsonObject(process.env.HOMEBOY_SETTINGS_JSON)) || {};
  const settingsRuntimePathKey = `wp_codebox_${LEGACY_RUNTIME_PREFIX}_path`;
  const settingsRuntimeToolsPathKey = `wp_codebox_${LEGACY_RUNTIME_PREFIX}_code_path`;
  const workspaceRoot = resolveWorkspaceRoot(request, config, inputs, settings, options);
  const workspaceBase = workspaceRoot ? path.dirname(workspaceRoot) : process.cwd();
  const dataMachinePath = firstExistingPath(
    options.agentRuntime,
    settings[settingsRuntimePathKey],
    settings[`${LEGACY_RUNTIME_PREFIX}_path`],
    process.env.HOMEBOY_DATA_MACHINE_PATH,
    activeSitePluginPath('data-machine'),
    siblingPath(workspaceBase, 'data-machine'),
  );
  const dataMachineCodePath = firstExistingPath(
    options.agentRuntimeTools,
    settings[settingsRuntimeToolsPathKey],
    settings[`${LEGACY_RUNTIME_PREFIX}_code_path`],
    process.env.HOMEBOY_DATA_MACHINE_CODE_PATH,
    activeSitePluginPath('data-machine-code'),
    siblingPath(workspaceBase, 'data-machine-code'),
  );
  const providerPluginPath = firstExistingPath(
    settings.wp_codebox_provider_plugin_path,
    process.env.HOMEBOY_WP_CODEBOX_PROVIDER_PLUGIN_PATH,
  );
  const provider = config.provider || options.provider || defaultProvider(settings, providerPluginPath);
  const model = config.model || options.model || defaultModelForProvider(provider, settings);
  const agentsApiPath = firstExistingPath(
    options.agentsApi,
    settings.wp_codebox_agents_api_path,
    settings.agents_api_path,
    process.env.HOMEBOY_WP_CODEBOX_AGENTS_API_PATH,
    bundledAgentsApiPath(dataMachinePath),
  );
  const phpAiClientPath = defaultPhpAiClientPath(settings, options);

  return {
    agentsApi: agentsApiPath,
    legacyRuntime: dataMachinePath,
    legacyRuntimeTools: dataMachineCodePath,
    providerPluginPaths: defaultProviderPluginPaths(provider, settings, providerPluginPath),
    provider,
    model,
    secretEnv: defaultSecretEnv(provider, settings),
    wpCodeboxBin: firstValue(settings.wp_codebox_bin, settings.wpCodeboxBin, process.env.HOMEBOY_WP_CODEBOX_BIN, ''),
    runtimeOverlayProfiles: defaultRuntimeOverlayProfiles(settings),
    runtimeOverlays: defaultRuntimeOverlays(settings, phpAiClientPath),
    runtimeEnv: defaultRuntimeEnv(settings),
    runtimeStateMounts: defaultRuntimeStateMounts(settings),
    runtimeConfigMounts: defaultRuntimeConfigMounts(settings),
    mounts: defaultWorkspaceMounts(workspaceRoot, request, config, inputs, options),
    workspaces: defaultWorkspaces(config, inputs, options),
    allowedTools: defaultWorkspaceAllowedTools(workspaceRoot, workspaceMode(request, config, inputs)),
    sandboxToolPolicy: defaultWorkspaceSandboxToolPolicy(workspaceRoot, workspaceMode(request, config, inputs)),
  };
}

function defaultProviderPluginPaths(provider, settings, fallbackProviderPluginPath) {
  const explicit = normalizeArray(settings.wp_codebox_provider_plugin_paths || settings.provider_plugin_paths);
  if (explicit.length > 0) {
    return explicit;
  }
  if (provider === 'codex') {
    return [];
  }
  return fallbackProviderPluginPath ? [fallbackProviderPluginPath] : [];
}

function defaultRuntimeOverlayProfiles(settings) {
  return normalizeArray(settings.wp_codebox_runtime_overlay_profiles || settings.runtime_overlay_profiles);
}

function defaultRuntimeOverlays(settings, phpAiClientPath = '') {
  const explicit = normalizeArray(settings.wp_codebox_runtime_overlays || settings.runtime_overlays);
  if (explicit.length > 0) {
    return explicit;
  }

  return phpAiClientPath ? [{
    kind: 'bundled-library',
    library: 'php-ai-client',
    source: phpAiClientPath,
    target: '/wordpress/wp-includes/php-ai-client',
    strategy: 'wordpress-scoped-bundle',
    metadata: { component: 'php-ai-client', source: 'homeboy-extensions-default' },
  }] : [];
}

function defaultPhpAiClientPath(settings, options = {}) {
  return firstExistingPath(
    options.phpAiClient,
    settings.wp_codebox_php_ai_client_path,
    settings.php_ai_client_path,
    process.env.HOMEBOY_WP_CODEBOX_PHP_AI_CLIENT_PATH,
    process.env.PHP_AI_CLIENT_PATH,
  );
}

function defaultRuntimeEnv(settings) {
  return firstObject(settings.wp_codebox_runtime_env, settings.runtime_env) || {};
}

function defaultRuntimeStateMounts(settings) {
  return normalizeArray(settings.wp_codebox_runtime_state_mounts || settings.runtime_state_mounts);
}

function defaultRuntimeConfigMounts(settings) {
  return normalizeArray(settings.wp_codebox_runtime_config_mounts || settings.runtime_config_mounts);
}

function firstDefined(...values) {
  return values.find((value) => value !== undefined);
}

function parseJsonObject(value) {
  if (!value || typeof value !== 'string') {
    return null;
  }
  try {
    const parsed = JSON.parse(value);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

function normalizeArray(value) {
  if (Array.isArray(value)) {
    return value.filter(Boolean);
  }
  return value ? [value] : [];
}

function firstExistingPath(...candidates) {
  for (const candidate of candidates.flatMap((value) => normalizeArray(value))) {
    if (candidate && fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return '';
}

function siblingPath(base, name) {
  return base && name ? path.join(base, name) : '';
}

function activeSitePluginPath(slug) {
  const candidate = path.join(process.cwd(), 'wp-content', 'plugins', slug);
  return fs.existsSync(candidate) ? candidate : '';
}

function bundledAgentsApiPath(dataMachinePath) {
  return dataMachinePath ? [
    path.join(dataMachinePath, 'vendor', 'wordpress', 'agents-api'),
    path.join(dataMachinePath, 'vendor', 'automattic', 'agents-api'),
  ] : [];
}

function defaultSecretEnv(provider, settings) {
  const explicit = normalizeArray(settings.wp_codebox_secret_env || settings.secret_env);
  if (explicit.length > 0) {
    return explicit;
  }
  if (provider === 'codex') {
    return CODEX_SECRET_ENV;
  }
  if (provider === 'claude-code') {
    return CLAUDE_CODE_SECRET_ENV;
  }
  return provider === 'openai' ? ['OPENAI_API_KEY'] : [];
}

function defaultProvider(settings, providerPluginPath) {
  const explicit = settings.wp_codebox_provider || settings.provider || process.env.HOMEBOY_WP_CODEBOX_PROVIDER;
  if (explicit) {
    return explicit;
  }
  return hasCodexSubscriptionAuth(settings) ? 'codex' : defaultProviderForPluginPath(providerPluginPath);
}

function defaultProviderForPluginPath(providerPluginPath) {
  if (!providerPluginPath) {
    return '';
  }
  return path.basename(providerPluginPath).startsWith('ai-provider-for-openai') ? 'openai' : '';
}

function defaultModelForProvider(provider, settings) {
  const explicit = settings.wp_codebox_model || settings.model || process.env.HOMEBOY_WP_CODEBOX_MODEL;
  if (explicit) {
    return explicit;
  }
  const codexModel = settings.wp_codebox_codex_model || process.env.HOMEBOY_WP_CODEBOX_CODEX_MODEL;
  if (provider === 'codex' && hasCodexSubscriptionAuth(settings)) {
    return codexModel || DEFAULT_CODEX_MODEL;
  }
  return '';
}

function hasCodexSubscriptionAuth(settings = {}) {
  if (settings.wp_codebox_codex_enabled === false || settings.wp_codebox_codex_enabled === 'false') {
    return false;
  }
  const envHasTokens = CODEX_SECRET_ENV.slice(0, 4).every((name) => Boolean(process.env[name]));
  if (envHasTokens) {
    return true;
  }
  const authPath = settings.wp_codebox_codex_auth_path || process.env.HOMEBOY_WP_CODEBOX_CODEX_AUTH_PATH || defaultCodexAuthPath();
  const auth = readJsonFile(authPath);
  return Boolean(auth?.tokens?.access_token && auth?.tokens?.refresh_token && auth?.tokens?.account_id);
}

function defaultCodexAuthPath() {
  const home = process.env.HOME;
  return home ? path.join(home, '.codex', 'auth.json') : '';
}

function readJsonFile(filePath) {
  if (!filePath || !fs.existsSync(filePath)) {
    return null;
  }
  try {
    return JSON.parse(fs.readFileSync(filePath, 'utf8'));
  } catch {
    return null;
  }
}

function defaultWorkspaceToolIds(workspaceRoot, workspaceModeValue) {
  if (!workspaceRoot) {
    return [];
  }
  if (workspaceModeValue === 'readwrite') {
    return [...DEFAULT_WORKSPACE_READONLY_TOOLS, ...DEFAULT_WORKSPACE_WRITE_TOOLS];
  }
  return [...DEFAULT_WORKSPACE_READONLY_TOOLS];
}

function defaultWorkspaceAllowedTools(workspaceRoot, workspaceModeValue) {
  return defaultWorkspaceToolIds(workspaceRoot, workspaceModeValue);
}

function defaultWorkspaceSandboxToolPolicy(workspaceRoot, workspaceModeValue) {
  if (!workspaceRoot) {
    return undefined;
  }
  return {
    schema: 'wp-codebox/sandbox-tool-policy/v1',
    version: 1,
    tools: defaultWorkspaceToolIds(workspaceRoot, workspaceModeValue).map((tool) => ({
      id: tool,
      runtime_tool_id: tool,
      execution_location: 'sandbox',
      transport_visibility: 'sandbox',
      allowed: true,
      runtime: {
        environment: 'runtime_local',
        capability_scope: 'runtime_local',
      },
    })),
    metadata: { source: 'homeboy.codebox-agent-task.default-workspace-tools' },
  };
}

function resolveWorkspaceRoot(request, config, inputs, settings, options) {
  const candidates = [
    options.workspaceRoot,
    inputs.target?.root,
    inputs.target?.path,
    request.workspace?.root,
    request.workspace?.path,
    config.workspace_root,
    config.workspaceRoot,
    settings.wp_codebox_workspace_root,
    process.env.HOMEBOY_COMPONENT_PATH,
  ];
  return firstExistingPath(...candidates) || '';
}

function workspaceMode(request, config, inputs) {
  const mode = inputs.target?.mode || request.workspace?.mode || config.workspace_mode || config.workspaceMode || 'readwrite';
  return mode === 'readonly' ? 'readonly' : 'readwrite';
}

function defaultWorkspaceMounts(workspaceRoot, request, config, inputs, options) {
  const explicit = config.mounts || options.mounts || [];
  const workspaceTarget = defaultWorkspaceTarget(workspaceRoot);
  if (!workspaceRoot || explicit.some((mount) => mount?.target === workspaceTarget || mount?.source === workspaceRoot)) {
    return explicit;
  }
  return [
    ...explicit,
    {
      source: workspaceRoot,
      target: workspaceTarget,
      mode: workspaceMode(request, config, inputs),
      metadata: { kind: 'homeboy-dmc-workspace', workspace_slug: workspaceSlug(workspaceRoot) },
    },
  ];
}

function defaultWorkspaceTarget(workspaceRoot) {
  const slug = workspaceSlug(workspaceRoot);
  return slug ? `/workspace/${slug}` : '/workspace';
}

function workspaceSlug(workspaceRoot) {
  if (!workspaceRoot) {
    return '';
  }
  const slug = path.basename(workspaceRoot).split('@')[0].replace(/[^A-Za-z0-9_-]/g, '-').replace(/^-+|-+$/g, '');
  return slug || 'workspace';
}

function defaultWorkspaces(config, inputs, options) {
  const explicit = inputs.workspaces || config.workspaces || options.workspaces || [];
  return explicit;
}

function agentBundleMounts(bundleConfig, explicitMounts = []) {
  const mounts = Array.isArray(explicitMounts) ? [...explicitMounts] : [];
  const source = bundleConfig?.bundle_host_path;
  const target = bundleConfig?.bundle_path;
  if (!source || !target) {
    return mounts;
  }
  if (mounts.some((mount) => mount?.source === source || mount?.target === target)) {
    return mounts;
  }
  return [
    ...mounts,
    {
      source,
      target,
      mode: 'readonly',
      metadata: { kind: 'agent-bundle' },
    },
  ];
}

function firstObject(...candidates) {
  return candidates.find((candidate) => candidate && typeof candidate === 'object' && !Array.isArray(candidate)) || null;
}

function firstValue(...candidates) {
  return candidates.find((candidate) => candidate !== undefined && candidate !== null && candidate !== '');
}

function recipeConfigFromAgentTaskRequest(request, config, inputs) {
  const explicit = firstObject(
    inputs.recipe,
    inputs.recipe_pack,
    inputs.recipePack,
    config.recipe,
    config.recipe_pack,
    config.recipePack,
  ) || {};
  const recipeValue = firstValue(
    inputs.recipe,
    inputs.recipe_name,
    inputs.recipeName,
    config.recipe,
    config.recipe_name,
    config.recipeName,
  );
  const sourceRefs = Array.isArray(request.source_refs) ? request.source_refs : [];
  const sourceRef = sourceRefs.find((ref) => ref && typeof ref === 'object' && (ref.kind === 'pull_request' || ref.kind === 'branch' || ref.kind === 'commit' || ref.uri || ref.ref)) || {};

  const recipe = Object.fromEntries(Object.entries({
    schema: 'wp-codebox/external-recipe-request/v1',
    pack: explicit.pack || explicit.package || firstValue(inputs.recipe_pack, inputs.recipePack, config.recipe_pack, config.recipePack),
    name: explicit.name || (typeof recipeValue === 'string' ? recipeValue : undefined),
    ref: explicit.ref || firstValue(inputs.recipe_ref, inputs.recipeRef, config.recipe_ref, config.recipeRef),
    path: explicit.path || firstValue(inputs.recipe_path, inputs.recipePath, config.recipe_path, config.recipePath),
    repository: explicit.repository || explicit.repo || firstValue(inputs.recipe_repo, inputs.recipeRepo, config.recipe_repo, config.recipeRepo),
    target_ref: explicit.target_ref || explicit.targetRef || firstValue(inputs.target_ref, inputs.targetRef, config.target_ref, config.targetRef, request.ref, sourceRef.ref, sourceRef.uri),
    target_repo: explicit.target_repo || explicit.targetRepo || firstValue(inputs.target_repo, inputs.targetRepo, config.target_repo, config.targetRepo, sourceRef.repo),
    target_pr: explicit.target_pr || explicit.targetPr || firstValue(inputs.target_pr, inputs.targetPr, config.target_pr, config.targetPr, sourceRef.pr, sourceRef.number),
    target_branch: explicit.target_branch || explicit.targetBranch || firstValue(inputs.target_branch, inputs.targetBranch, config.target_branch, config.targetBranch),
    inputs: explicit.inputs || inputs.recipe_inputs || inputs.recipeInputs || config.recipe_inputs || config.recipeInputs,
    secret_env: explicit.secret_env || explicit.secretEnv || inputs.recipe_secret_env || inputs.recipeSecretEnv || config.recipe_secret_env || config.recipeSecretEnv,
    metadata: explicit.metadata,
  }).filter(([, value]) => value !== undefined && value !== '' && !(Array.isArray(value) && value.length === 0)));

  return recipe.pack || recipe.name || recipe.path || recipe.repository || recipe.target_ref ? recipe : {};
}

function agentBundleConfigFromAgentTaskRequest(request, config, inputs) {
  const explicitBundleSources = [
    inputs.agent_bundle,
    inputs.agentBundle,
    ...LEGACY_BUNDLE_KEYS.map((key) => inputs[key]),
    config.agent_bundle,
    config.agentBundle,
    ...LEGACY_BUNDLE_KEYS.map((key) => config[key]),
  ].filter((value) => value && typeof value === 'object');
  const requestedBundle = config.execution_kind === 'agent_bundle'
    || inputs.execution_kind === 'agent_bundle'
    || explicitBundleSources.length > 0
    || AGENT_BUNDLE_TRIGGER_FIELDS.some((field) => inputs[field] !== undefined || config[field] !== undefined);

  if (!requestedBundle) {
    return {};
  }

  const candidateSources = [
    ...explicitBundleSources,
    inputs,
    config,
  ].filter((value) => value && typeof value === 'object');
  const bundleConfig = {};
  for (const field of AGENT_BUNDLE_CONFIG_FIELDS) {
    for (const source of candidateSources) {
      if (source[field] !== undefined) {
        bundleConfig[field] = source[field];
        break;
      }
    }
  }
  bundleConfig.prompt = bundleConfig.prompt || request.instructions;
  bundleConfig.provider = bundleConfig.provider || config.provider || '';
  bundleConfig.model = bundleConfig.model || request.executor?.model || config.model || '';
  if (!bundleConfig.provider_plugin_paths && config.provider_plugin_paths) {
    bundleConfig.provider_plugin_paths = config.provider_plugin_paths;
  }
  return Object.fromEntries(Object.entries(bundleConfig).filter(([, value]) => value !== undefined && value !== ''));
}

function normalizeStatus(result) {
  if (recipeRunFailedPhase(recipeRunFromResult(result))) {
    return 'failed';
  }
  if (result?.outputs && typeof result.outputs === 'object' && result.outputs.success === false) {
    return 'failed';
  }
  const workload = agentRuntimeWorkload(result);
  if (workload && workload.success === false) {
    return 'failed';
  }
  if (AGENT_TASK_OUTCOME_STATUSES.includes(result?.status)) {
    return result.status;
  }
  if (result?.status === 'completed') {
    return result?.success === true ? 'succeeded' : 'failed';
  }
  const agentResult = result?.run?.agentResult || result?.agentResult || result?.agent_result || result?.metadata?.recipe_run?.agentResult || result?.metadata?.recipe_run?.run?.agentResult;
  const changedFileCount = agentResult?.changedFiles?.count;
  const patchBytes = agentResult?.patch?.bytes;
  if (result?.success === true && agentResult?.noOpReason && changedFileCount === 0 && patchBytes === 0) {
    return 'no_op';
  }
  if (result?.outcome === 'no_op' || result?.no_op) {
    return 'no_op';
  }
  if (result?.unable_to_remediate) {
    return 'unable_to_remediate';
  }
  if (result?.timeout) {
    return 'timeout';
  }
  if (result?.provider_error) {
    return 'provider_error';
  }
  return result?.success === true ? 'succeeded' : 'failed';
}

function agentRuntimeWorkload(result) {
  return result?.raw?.agent_runtime?.result || result?.metadata?.agent_runtime?.workload || result?.run?.agentResult || result?.agentResult || result?.agent_result || null;
}

function appendUniqueArtifact(artifacts, artifact) {
  if (!artifact || !artifact.kind) {
    return;
  }
  const key = artifact.path || artifact.url || artifact.id;
  if (key && artifacts.some((existing) => (existing.path || existing.url || existing.id) === key)) {
    return;
  }
  artifacts.push(artifact);
}

function appendUniqueEvidenceRef(refs, ref) {
  if (!ref || !ref.uri) {
    return;
  }
  if (refs.some((existing) => existing.uri === ref.uri && existing.kind === ref.kind)) {
    return;
  }
  refs.push(ref);
}

function artifactPath(root, relativePath) {
  if (!root || !relativePath) {
    return '';
  }
  return `${String(root).replace(/\/$/, '')}/${String(relativePath).replace(/^\//, '')}`;
}

function codeboxBundleArtifacts(result) {
  const artifacts = [];
  const artifactRefs = Array.isArray(result.run?.artifactRefs) ? result.run.artifactRefs : [];
  for (const ref of artifactRefs) {
    appendUniqueArtifact(artifacts, {
      id: ref.id || ref.digest?.value,
      kind: ref.kind || 'codebox-artifact-bundle',
      path: ref.directory,
      sha256: ref.digest?.value,
      metadata: { digest: ref.digest },
    });
  }

  const completionOutcome = result.completionOutcome || result.completion_outcome || {};
  const bundleDirectory = result.run?.agentResult?.artifacts?.directory || result.agent_result?.artifacts?.directory || completionOutcome?.provenance?.artifactDirectory || result.session?.artifacts?.path;
  const artifactBundleId = completionOutcome?.provenance?.artifactBundleId || result.session?.artifacts?.bundle_id || result.artifacts?.id;
  appendUniqueArtifact(artifacts, {
    id: artifactBundleId,
    kind: 'codebox-artifact-bundle',
    path: bundleDirectory,
    metadata: {
      runtime_id: result.run?.runtime?.id,
      runtime_status: result.run?.runtime?.status,
    },
  });

  const agentResult = result.run?.agentResult || result.agentResult || result.agent_result || result.metadata?.recipe_run?.agentResult || {};
  const changedFilesPath = artifactPath(bundleDirectory, agentResult.changedFiles?.artifact || '');
  appendUniqueArtifact(artifacts, {
    id: changedFilesPath ? 'codebox-changed-files' : '',
    kind: 'codebox-changed-files',
    path: changedFilesPath,
    metadata: agentResult.changedFiles || {},
  });

  const patchPath = artifactPath(bundleDirectory, agentResult.patch?.artifact || '');
  appendUniqueArtifact(artifacts, {
    id: patchPath ? 'codebox-patch' : '',
    kind: 'codebox-patch',
    path: patchPath,
    sha256: agentResult.patch?.sha256,
    size_bytes: agentResult.patch?.bytes,
    metadata: agentResult.patch || {},
  });

  const transcriptPath = artifactPath(bundleDirectory, agentResult.transcript?.artifact || '');
  appendUniqueArtifact(artifacts, {
    id: transcriptPath ? 'codebox-transcript' : '',
    kind: 'codebox-transcript',
    path: transcriptPath,
    metadata: agentResult.transcript || {},
  });

  const runtimeLogPath = result.artifacts?.runtimeLogPath;
  appendUniqueArtifact(artifacts, {
    id: runtimeLogPath ? 'codebox-runtime-log' : '',
    kind: 'codebox-runtime-log',
    path: runtimeLogPath,
  });

  const commandsLogPath = result.artifacts?.commandsLogPath;
  appendUniqueArtifact(artifacts, {
    id: commandsLogPath ? 'codebox-command-log' : '',
    kind: 'codebox-command-log',
    path: commandsLogPath,
  });

  return artifacts;
}

function recipeRunFromResult(result) {
  return firstObject(
    result?.recipe_run,
    result?.recipeRun,
    result?.metadata?.recipe_run,
    result?.metadata?.recipeRun,
    result?.run?.recipe_run,
    result?.run?.recipeRun,
  ) || {};
}

function recipeRunFailedPhase(recipeRun) {
  if (!recipeRun || Object.keys(recipeRun).length === 0) {
    return '';
  }
  if (recipeRun.startup?.success === false || recipeRun.startup_failed || recipeRun.startupFailed) {
    return 'startup';
  }
  const probes = Array.isArray(recipeRun.probes) ? recipeRun.probes : [];
  if (recipeRun.probe?.success === false || recipeRun.probe_failed || recipeRun.probeFailed || probes.some((probe) => probe?.success === false || probe?.status === 'failed')) {
    return 'probe';
  }
  if (recipeRun.artifact_collection?.success === false || recipeRun.artifactCollection?.success === false || recipeRun.artifact_collection_failed || recipeRun.artifactCollectionFailed) {
    return 'artifact_collection';
  }
  return '';
}

function recipeRunFailureSummary(recipeRun) {
  const failedPhase = recipeRunFailedPhase(recipeRun);
  if (failedPhase === 'startup') {
    return recipeRun.startup?.summary || recipeRun.startup?.message || 'WP Codebox recipe startup failed.';
  }
  if (failedPhase === 'probe') {
    const failedProbe = (Array.isArray(recipeRun.probes) ? recipeRun.probes : []).find((probe) => probe?.success === false || probe?.status === 'failed') || recipeRun.probe || {};
    const label = failedProbe.name || failedProbe.id || failedProbe.path || 'recipe probe';
    return failedProbe.summary || failedProbe.message || `WP Codebox ${label} failed.`;
  }
  if (failedPhase === 'artifact_collection') {
    return recipeRun.artifact_collection?.summary || recipeRun.artifactCollection?.summary || recipeRun.artifact_collection?.message || recipeRun.artifactCollection?.message || 'WP Codebox recipe artifact collection failed.';
  }
  return '';
}

function recipeRunFailureDiagnostic(recipeRun) {
  const failedPhase = recipeRunFailedPhase(recipeRun);
  if (!failedPhase) {
    return null;
  }
  return {
    class: `codebox.recipe.${failedPhase}.failed`,
    message: recipeRunFailureSummary(recipeRun),
    data: sanitizePublicMetadata({ phase: failedPhase, recipe_run: recipeRun }),
  };
}

function appendRecipeArtifact(artifacts, artifact, fallbackKind, index) {
  if (!artifact) {
    return;
  }
  if (typeof artifact === 'string') {
    appendUniqueArtifact(artifacts, {
      id: artifact,
      kind: fallbackKind,
      path: artifact,
    });
    return;
  }
  appendUniqueArtifact(artifacts, {
    id: artifact.id || artifact.name || artifact.path || artifact.url || `${fallbackKind}-${index + 1}`,
    kind: artifact.kind || artifact.type || fallbackKind,
    name: artifact.name,
    path: artifact.path || artifact.file || artifact.directory,
    url: artifact.url,
    mime: artifact.mime,
    size_bytes: artifact.size_bytes || artifact.sizeBytes,
    sha256: artifact.sha256,
    metadata: artifact.metadata || {},
  });
}

function recipeRunArtifacts(result) {
  const recipeRun = recipeRunFromResult(result);
  if (!recipeRun || Object.keys(recipeRun).length === 0) {
    return [];
  }

  const artifacts = [];
  const startupLogs = [recipeRun.startup_log, recipeRun.startupLog, recipeRun.startup?.log, recipeRun.startup?.log_path, recipeRun.startup?.logPath].filter(Boolean);
  startupLogs.forEach((artifact, index) => appendRecipeArtifact(artifacts, artifact, 'codebox-recipe-startup-log', index));

  const probeJson = [recipeRun.probe_json, recipeRun.probeJson, recipeRun.probe_results, recipeRun.probeResults, recipeRun.probe?.artifact, recipeRun.probe?.path].filter(Boolean);
  probeJson.forEach((artifact, index) => appendRecipeArtifact(artifacts, artifact, 'codebox-recipe-probe-json', index));

  const probes = Array.isArray(recipeRun.probes) ? recipeRun.probes : [];
  probes.forEach((probe, index) => {
    appendRecipeArtifact(artifacts, probe.artifact || probe.path || probe.result_path || probe.resultPath, 'codebox-recipe-probe-json', index);
    appendRecipeArtifact(artifacts, probe.screenshot || probe.screenshot_path || probe.screenshotPath, 'codebox-recipe-screenshot', index);
  });

  const screenshots = [
    ...(Array.isArray(recipeRun.screenshots) ? recipeRun.screenshots : []),
    ...(Array.isArray(recipeRun.browser_screenshots) ? recipeRun.browser_screenshots : []),
    ...(Array.isArray(recipeRun.browserScreenshots) ? recipeRun.browserScreenshots : []),
  ];
  screenshots.forEach((artifact, index) => appendRecipeArtifact(artifacts, artifact, 'codebox-recipe-screenshot', index));

  const sideEffects = [recipeRun.fake_side_effects, recipeRun.fakeSideEffects, recipeRun.side_effects, recipeRun.sideEffects].filter(Boolean);
  sideEffects.forEach((artifact, index) => appendRecipeArtifact(artifacts, artifact, 'codebox-recipe-fake-side-effects', index));

  const declaredArtifacts = [
    ...(Array.isArray(recipeRun.artifacts) ? recipeRun.artifacts : []),
    ...(Array.isArray(recipeRun.declared_artifacts) ? recipeRun.declared_artifacts : []),
    ...(Array.isArray(recipeRun.declaredArtifacts) ? recipeRun.declaredArtifacts : []),
  ];
  declaredArtifacts.forEach((artifact, index) => appendRecipeArtifact(artifacts, artifact, 'codebox-recipe-artifact', index));

  return artifacts;
}

function homeboyFailureClassification(classification, status) {
  if (classification === 'provider' || classification === 'timeout') {
    return classification;
  }
  if (classification === 'runtime' || classification === 'task') {
    return 'execution_failed';
  }
  return classification || failureClassificationForStatus(status);
}

function failureClassificationForStatus(status) {
  if (status === 'provider_error') {
    return 'provider';
  }
  if (status === 'timeout') {
    return 'timeout';
  }
  if (status === 'unable_to_remediate') {
    return 'execution_failed';
  }
  if (status === 'failed') {
    return 'execution_failed';
  }
  return undefined;
}

function sanitizePublicMetadata(value) {
  if (Array.isArray(value)) {
    return value.map(sanitizePublicMetadata);
  }
  if (!value || typeof value !== 'object') {
    return value;
  }

  return Object.fromEntries(Object.entries(value).map(([key, entry]) => {
    if (AGENT_TASK_REDACTED_METADATA_KEYS.some((redactedKey) => redactedKey.toLowerCase() === key.toLowerCase())) {
      return [key, '[redacted]'];
    }
    return [key, sanitizePublicMetadata(entry)];
  }));
}

function normalizeTypedArtifactEntry(name, artifact) {
  if (!artifact || typeof artifact !== 'object' || Array.isArray(artifact)) {
    return null;
  }
  const artifactName = artifact.name || name;
  if (!artifactName) {
    return null;
  }
  const fileRefs = typedArtifactOutputFileRefs(artifact);
  return sanitizePublicMetadata(Object.fromEntries(Object.entries({
    schema: 'homeboy/agent-task-typed-artifact/v1',
    name: artifactName,
    type: artifact.type || artifact.kind || artifact.artifact_type || artifact.artifactType,
    artifact_schema: artifact.artifact_schema || artifact.artifactSchema || artifact.schema,
    payload: artifact.payload !== undefined ? artifact.payload : artifact.data,
    provenance: artifact.provenance && typeof artifact.provenance === 'object' && !Array.isArray(artifact.provenance) ? artifact.provenance : {},
    file_refs: fileRefs,
    metadata: artifact.metadata && typeof artifact.metadata === 'object' && !Array.isArray(artifact.metadata) ? artifact.metadata : {},
  }).filter(([, value]) => value !== undefined && value !== '' && !(Array.isArray(value) && value.length === 0))));
}

function typedArtifactOutputFileRefs(artifact) {
  if (Array.isArray(artifact.file_refs)) {
    return artifact.file_refs;
  }
  if (Array.isArray(artifact.fileRefs)) {
    return artifact.fileRefs;
  }
  return [];
}

function normalizeTypedArtifacts(value) {
  if (Array.isArray(value)) {
    return Object.fromEntries(value
      .map((artifact, index) => normalizeTypedArtifactEntry(artifact?.name || artifact?.id || `artifact_${index + 1}`, artifact))
      .filter(Boolean)
      .map((artifact) => [artifact.name, artifact]));
  }
  if (!value || typeof value !== 'object') {
    return {};
  }
  return Object.fromEntries(Object.entries(value)
    .map(([name, artifact]) => normalizeTypedArtifactEntry(name, artifact))
    .filter(Boolean)
    .map((artifact) => [artifact.name, artifact]));
}

function typedArtifactsFromResult(result) {
  const workload = agentRuntimeWorkload(result) || {};
  const scenarios = Array.isArray(workload.scenarios) ? workload.scenarios : [];
  const candidates = [
    result.outputs?.typed_artifacts,
    result.outputs?.typedArtifacts,
    result.run?.agentResult?.typed_artifacts,
    result.run?.agentResult?.typedArtifacts,
    result.run?.agentResult?.outputs?.typed_artifacts,
    result.run?.agentResult?.outputs?.typedArtifacts,
    result.agentResult?.outputs?.typed_artifacts,
    result.agentResult?.outputs?.typedArtifacts,
    result.agent_result?.outputs?.typed_artifacts,
    result.agent_result?.outputs?.typedArtifacts,
    result.metadata?.agent_runtime?.result?.typed_artifacts,
    result.metadata?.agent_runtime?.result?.typedArtifacts,
    result.metadata?.agent_runtime?.result?.outputs?.typed_artifacts,
    result.metadata?.agent_runtime?.result?.outputs?.typedArtifacts,
    workload.typed_artifacts,
    workload.typedArtifacts,
    workload.outputs?.typed_artifacts,
    workload.outputs?.typedArtifacts,
    ...scenarios.map((scenario) => scenario?.typed_artifacts),
    ...scenarios.map((scenario) => scenario?.typedArtifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.typed_artifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.typedArtifacts),
  ];
  return Object.assign({}, ...candidates.map(normalizeTypedArtifacts));
}

function typedArtifactFileRefs(typedArtifact) {
  const refs = Array.isArray(typedArtifact.file_refs) ? typedArtifact.file_refs : [];
  const directRefs = [typedArtifact.path, typedArtifact.url, typedArtifact.file, typedArtifact.directory].filter(Boolean);
  return [
    ...directRefs.map((ref) => ({ path: ref })),
    ...refs,
  ];
}

function typedArtifactNameFromDeclaration(declaration) {
  if (typeof declaration === 'string') {
    return declaration;
  }
  if (!declaration || typeof declaration !== 'object' || Array.isArray(declaration)) {
    return '';
  }
  return declaration.name || declaration.id || '';
}

function requiredArtifactDeclarationsFromRequest(request) {
  const config = request.executor?.config || {};
  return artifactDeclarationsFromAgentTaskRequest(request, config, request.inputs || {})
    .filter((declaration) => declaration && typeof declaration === 'object' && declaration.required === true && typedArtifactNameFromDeclaration(declaration));
}

function artifactDeclarationsMetadataFromRequest(request) {
  const config = request.executor?.config || {};
  return artifactDeclarationsFromAgentTaskRequest(request, config, request.inputs || {});
}

function missingRequiredTypedArtifactDiagnostic(request, outputs) {
  const required = requiredArtifactDeclarationsFromRequest(request);
  if (required.length === 0) {
    return null;
  }
  const typedArtifacts = outputs?.typed_artifacts && typeof outputs.typed_artifacts === 'object' && !Array.isArray(outputs.typed_artifacts)
    ? outputs.typed_artifacts
    : {};
  const missing = required
    .map((declaration) => ({
      name: typedArtifactNameFromDeclaration(declaration),
      type: declaration.type || declaration.kind || declaration.artifact_type || declaration.artifactType || '',
      artifact_schema: declaration.artifact_schema || declaration.artifactSchema || declaration.schema || '',
    }))
    .filter((declaration) => !typedArtifacts[declaration.name]);
  if (missing.length === 0) {
    return null;
  }
  return {
    class: 'codebox.required_typed_artifacts_missing',
    message: `WP Codebox agent task did not produce required typed artifacts: ${missing.map((declaration) => declaration.name).join(', ')}.`,
    data: { reason: 'missing_required_typed_artifacts', missing },
  };
}

function typedBundleOutputArtifacts(result) {
  return Object.values(typedArtifactsFromResult(result)).flatMap((typedArtifact) => typedArtifactFileRefs(typedArtifact).map((ref, index) => {
    const fileRef = typeof ref === 'string' ? { path: ref } : ref;
    if (!fileRef || typeof fileRef !== 'object') {
      return null;
    }
    return {
      id: fileRef.id || `${typedArtifact.name}-${index + 1}`,
      kind: 'typed-bundle-output',
      name: typedArtifact.name,
      path: fileRef.path || fileRef.file || fileRef.directory,
      url: fileRef.url,
      mime: fileRef.mime,
      sha256: fileRef.sha256,
      metadata: {
        type: typedArtifact.type,
        artifact_schema: typedArtifact.artifact_schema,
        provenance: typedArtifact.provenance,
        file_ref: fileRef,
      },
    };
  }).filter(Boolean));
}

function artifactFromCodeboxArtifact(artifact, index) {
  const id = artifact.id || artifact.sha256 || artifact.path || artifact.url || `codebox-artifact-${index + 1}`;
  return {
    schema: AGENT_TASK_ARTIFACT_SCHEMA,
    id,
    kind: artifact.kind || artifact.type || 'codebox_artifact',
    name: artifact.name,
    path: artifact.path || artifact.directory,
    url: artifact.url,
    mime: artifact.mime,
    size_bytes: artifact.size_bytes,
    sha256: artifact.sha256,
    metadata: sanitizePublicMetadata(artifact.metadata || {}),
  };
}

function pathValue(source, dottedPath) {
  return String(dottedPath || '').split('.').filter(Boolean).reduce((value, key) => (value && typeof value === 'object' ? value[key] : undefined), source);
}

function normalizeOutputs(result) {
  const workload = agentRuntimeWorkload(result) || {};
  const typedArtifacts = typedArtifactsFromResult(result);
  const appendTypedArtifacts = (outputs) => Object.keys(typedArtifacts).length > 0
    ? {
        ...outputs,
        typed_artifacts: {
          ...(outputs.typed_artifacts && typeof outputs.typed_artifacts === 'object' && !Array.isArray(outputs.typed_artifacts) ? outputs.typed_artifacts : {}),
          ...typedArtifacts,
        },
      }
    : outputs;

  const bundle = result.metadata?.agent_runtime?.bundle || result.task_input?.agent_bundle || {};
  const configuredOutputs = bundle.engine_data_outputs && typeof bundle.engine_data_outputs === 'object' ? bundle.engine_data_outputs : {};
  if (Object.keys(configuredOutputs).length === 0 && workload.outputs && typeof workload.outputs === 'object' && !Array.isArray(workload.outputs)) {
    return sanitizePublicMetadata(appendTypedArtifacts(workload.outputs));
  }
  if (Object.keys(configuredOutputs).length === 0 && result.outputs && typeof result.outputs === 'object' && !Array.isArray(result.outputs)) {
    return sanitizePublicMetadata(appendTypedArtifacts(result.outputs));
  }
  const scenarios = Array.isArray(workload.scenarios) ? workload.scenarios : [];
  const configuredOutputSources = [
    ...scenarios,
    result.run?.agentResult,
    result.agentResult,
    result.agent_result,
    result.metadata?.agent_runtime?.result,
    result.outputs,
    result.run?.agentResult?.outputs,
    result.agentResult?.outputs,
    result.agent_result?.outputs,
    result.metadata?.agent_runtime?.result?.outputs,
    workload,
    workload.outputs,
  ].filter((source) => source && typeof source === 'object' && !Array.isArray(source));
  const outputSources = configuredOutputSources.flatMap((source) => [source, { metadata: source }]);
  const outputs = {};
  for (const [name, outputPath] of Object.entries(configuredOutputs)) {
    for (const source of outputSources) {
      const value = pathValue(source, outputPath);
      if (value !== undefined && value !== null && value !== '') {
        outputs[name] = value;
        break;
      }
    }
  }
  const fallbackOutputs = workload.outputs && typeof workload.outputs === 'object' && !Array.isArray(workload.outputs)
    ? workload.outputs
    : result.outputs || {};
  return sanitizePublicMetadata(appendTypedArtifacts(Object.keys(outputs).length > 0 ? outputs : fallbackOutputs));
}

function outputEvidenceRefs(outputs) {
  return Object.entries(outputs || {})
    .filter(([name, value]) => /(?:^|_)url$/i.test(name) && typeof value === 'string' && /^https?:\/\//i.test(value))
    .map(([name, value]) => ({
      kind: `agent-output-${name.replace(/_/g, '-')}`,
      uri: value,
      label: name.replace(/_/g, ' '),
    }));
}

function codeboxRunSummary(result, options = {}) {
  if (typeof options.normalizeAgentTaskRunResult !== 'function') {
    return null;
  }
  try {
    return enrichFailedCodeboxRunSummary(
      options.normalizeAgentTaskRunResult(result, { exitStatus: options.exitStatus ?? 0 }),
      result,
      options,
    );
  } catch {
    return null;
  }
}

function enrichFailedCodeboxRunSummary(runSummary, result = {}, options = {}) {
  if (!runSummary || typeof runSummary !== 'object' || Array.isArray(runSummary)) {
    return runSummary;
  }
  if (runSummary.status !== 'failed' || codeboxRunSummaryHasActionableRefs(runSummary, result)) {
    return runSummary;
  }

  const diagnostic = {
    class: 'codebox.no_runtime_session',
    message: 'WP Codebox reported a failed agent task without a run id, runtime id, session id, logs, transcripts, or artifact refs.',
    data: sanitizePublicMetadata({
      exit_status: options.exitStatus ?? 0,
      result_status: result.status,
      result_schema: result.schema,
      has_run: !!result.run,
      has_session: !!result.session,
    }),
  };

  return {
    ...runSummary,
    diagnostics: [diagnostic, ...(Array.isArray(runSummary.diagnostics) ? runSummary.diagnostics : [])],
    metadata: {
      ...(runSummary.metadata && typeof runSummary.metadata === 'object' && !Array.isArray(runSummary.metadata) ? runSummary.metadata : {}),
      provider_error: {
        code: 'codebox_no_runtime_session',
        message: 'WP Codebox failed before reporting a runtime/session or evidence refs.',
        exit_status: options.exitStatus ?? 0,
      },
    },
  };
}

function codeboxRunSummaryHasActionableRefs(runSummary, result = {}) {
  const metadata = runSummary.metadata && typeof runSummary.metadata === 'object' && !Array.isArray(runSummary.metadata) ? runSummary.metadata : {};
  if ([runSummary.run_id, runSummary.runtime_id, metadata.run_id, metadata.runtime_id, result.run?.runId, result.run?.runtime?.id, result.session?.id].some(Boolean)) {
    return true;
  }
  if (Array.isArray(runSummary.artifacts) && runSummary.artifacts.length > 0) {
    return true;
  }
  if (Array.isArray(runSummary.diagnostics) && runSummary.diagnostics.length > 0) {
    return true;
  }
  const refs = runSummary.refs && typeof runSummary.refs === 'object' && !Array.isArray(runSummary.refs) ? runSummary.refs : {};
  return Object.values(refs).some((value) => Array.isArray(value) && value.length > 0);
}

function codeboxRecipeRunSummary(result, options = {}) {
  if (typeof options.normalizeRecipeRunSummary !== 'function') {
    return null;
  }
  const recipeRun = recipeRunFromResult(result);
  if (!recipeRun || Object.keys(recipeRun).length === 0) {
    return null;
  }
  try {
    return options.normalizeRecipeRunSummary(recipeRun, { exitStatus: options.exitStatus ?? 0 });
  } catch {
    return null;
  }
}

function normalizeArtifacts(result, runSummary = null, recipeSummary = null) {
  const normalizedArtifacts = Array.isArray(runSummary?.artifacts)
    ? runSummary.artifacts.map(artifactFromCodeboxArtifact)
    : [];
  if (Array.isArray(recipeSummary?.artifacts)) {
    recipeSummary.artifacts.map(artifactFromCodeboxArtifact).forEach((artifact) => appendUniqueArtifact(normalizedArtifacts, artifact));
  }

  if (result?.schema === 'wp-codebox/agent-task-run/v1') {
    const artifacts = [...normalizedArtifacts];
    if (typeof result.artifacts === 'string' && result.artifacts) {
      artifacts.push({
        id: result.session?.artifacts?.bundle_id || 'wp-codebox-artifacts',
        kind: 'codebox-artifact-directory',
        path: result.artifacts,
        metadata: {
          session_id: result.session?.id,
          preview_url: result.session?.artifacts?.preview_url,
        },
      });
    }
    if (result.session?.artifacts && typeof result.session.artifacts === 'object') {
      artifacts.push({
        id: result.session.artifacts.bundle_id || `wp-codebox-session-artifacts-${artifacts.length + 1}`,
        kind: 'codebox-session-artifacts',
        url: result.session.artifacts.preview_url,
        metadata: result.session.artifacts,
      });
    }
    if (Array.isArray(result.artifacts)) {
      result.artifacts.map(artifactFromCodeboxArtifact).forEach((artifact) => appendUniqueArtifact(artifacts, artifact));
    }
    for (const artifact of codeboxBundleArtifacts(result)) {
      appendUniqueArtifact(artifacts, artifact);
    }
    for (const artifact of agentRuntimeBundleArtifacts(result)) {
      appendUniqueArtifact(artifacts, artifact);
    }
    for (const artifact of typedBundleOutputArtifacts(result)) {
      appendUniqueArtifact(artifacts, artifact);
    }
    if (!recipeSummary) {
      for (const artifact of recipeRunArtifacts(result)) {
        appendUniqueArtifact(artifacts, artifact);
      }
    }
    return artifacts.map(artifactFromCodeboxArtifact);
  }
  const artifacts = Array.isArray(result?.artifacts)
    ? result.artifacts
    : Object.values(result?.artifacts || {}).filter((value) => value && typeof value === 'object');
  const mappedArtifacts = [...normalizedArtifacts];
  artifacts.map(artifactFromCodeboxArtifact).forEach((artifact) => appendUniqueArtifact(mappedArtifacts, artifact));
  return mappedArtifacts;
}

function normalizeEvidenceRefs(result, runSummary = null, recipeSummary = null) {
  if (result?.schema === 'wp-codebox/agent-task-run/v1') {
    const refs = [
      result.session?.artifacts?.preview_url ? {
        kind: 'codebox-preview',
        uri: result.session.artifacts.preview_url,
        label: 'WP Codebox preview',
      } : null,
      typeof result.artifacts === 'string' && result.artifacts ? {
        kind: 'codebox-artifact-directory',
        uri: result.artifacts,
        label: 'WP Codebox artifacts',
      } : null,
    ].filter(Boolean);
    for (const artifact of codeboxBundleArtifacts(result)) {
      appendUniqueEvidenceRef(refs, {
        kind: artifact.kind,
        uri: artifact.path || artifact.url,
        label: artifact.kind.replace(/^codebox-/, 'WP Codebox ').replace(/-/g, ' '),
      });
    }
    for (const artifact of agentRuntimeBundleArtifacts(result)) {
      appendUniqueEvidenceRef(refs, {
        kind: artifact.kind,
        uri: artifact.path || artifact.url,
        label: artifact.kind.replace(/^agent-runtime-/, 'Agent runtime ').replace(/-/g, ' '),
      });
    }
    for (const artifact of typedBundleOutputArtifacts(result)) {
      appendUniqueEvidenceRef(refs, {
        kind: artifact.kind,
        uri: artifact.path || artifact.url,
        label: `Typed bundle output ${artifact.name || ''}`.trim(),
      });
    }
    if (!recipeSummary) {
      for (const artifact of recipeRunArtifacts(result)) {
        appendUniqueEvidenceRef(refs, {
          kind: artifact.kind,
          uri: artifact.path || artifact.url,
          label: artifact.kind.replace(/^codebox-recipe-/, 'WP Codebox recipe ').replace(/-/g, ' '),
        });
      }
    }
    for (const artifact of runSummary?.artifacts || []) {
      appendUniqueEvidenceRef(refs, {
        kind: artifact.kind,
        uri: artifact.path || artifact.url,
        label: artifact.kind.replace(/^codebox-/, 'WP Codebox ').replace(/-/g, ' '),
      });
    }
    for (const artifact of recipeSummary?.artifacts || []) {
      appendUniqueEvidenceRef(refs, {
        kind: artifact.kind,
        uri: artifact.path || artifact.url,
        label: artifact.kind.replace(/^codebox-/, 'WP Codebox ').replace(/-/g, ' '),
      });
    }
    for (const ref of outputEvidenceRefs(normalizeOutputs(result))) {
      appendUniqueEvidenceRef(refs, ref);
    }
    for (const ref of result?.evidence_refs || result?.evidence || []) {
      appendUniqueEvidenceRef(refs, {
        kind: ref.kind || ref.type || 'codebox_evidence',
        uri: ref.uri || ref.url || ref.path,
        label: ref.label || ref.name,
      });
    }
    return refs;
  }
  const evidenceRefs = result?.evidence_refs || result?.evidence || [];
  return evidenceRefs.map((ref) => ({
    kind: ref.kind || ref.type || 'codebox_evidence',
    uri: ref.uri || ref.url || ref.path,
    label: ref.label || ref.name,
  })).filter((ref) => ref.uri);
}

function agentRuntimeBundleArtifacts(result) {
  const artifacts = [];
  const workload = agentRuntimeWorkload(result) || {};
  const scenarios = Array.isArray(workload.scenarios) ? workload.scenarios : [];
  for (const scenario of scenarios) {
    const metadata = scenario?.metadata || {};
    const transcript = metadata.transcript_artifacts || {};
    appendUniqueArtifact(artifacts, {
      id: transcript.json ? 'agent-runtime-transcript-json' : '',
      kind: 'agent-runtime-transcript',
      path: transcript.json,
      metadata: { scenario_id: scenario.id, format: 'json' },
    });
    appendUniqueArtifact(artifacts, {
      id: transcript.summary ? 'agent-runtime-transcript-summary' : '',
      kind: 'agent-runtime-transcript-summary',
      path: transcript.summary,
      metadata: { scenario_id: scenario.id, format: 'markdown' },
    });
    const replayBundlePath = metadata.replay_bundle_path || metadata.replay_bundle?.path;
    appendUniqueArtifact(artifacts, {
      id: replayBundlePath ? 'agent-runtime-replay-bundle' : '',
      kind: 'agent-runtime-replay-bundle',
      path: replayBundlePath,
      metadata: { scenario_id: scenario.id },
    });
    const exports = Array.isArray(metadata.job_artifact_exports) ? metadata.job_artifact_exports : [];
    for (const [index, exported] of exports.entries()) {
      appendUniqueArtifact(artifacts, {
        id: exported.id || exported.path || `agent-runtime-job-artifact-${index + 1}`,
        kind: exported.kind || 'agent-runtime-job-artifact',
        path: exported.path,
        url: exported.url,
        metadata: { ...exported, scenario_id: scenario.id },
      });
    }
    const runnerPublicationUrl = Array.isArray(metadata.engine_data?.runner_publications)
      ? metadata.engine_data.runner_publications.find((publication) => publication?.url)?.url
      : '';
    const pullRequestUrl = metadata.runner_workspace_publication?.url || metadata.runner_workspace_publication?.html_url || metadata.runner_workspace_publication?.result?.url || metadata.runner_workspace_publication?.result?.html_url || runnerPublicationUrl || metadata.engine_data?.pull_request?.url || metadata.engine_data?.static_site_agent?.pr_url;
    appendUniqueArtifact(artifacts, {
      id: pullRequestUrl ? 'agent-runtime-pull-request' : '',
      kind: 'agent-runtime-pull-request',
      url: pullRequestUrl,
      metadata: { scenario_id: scenario.id },
    });
  }
  return artifacts;
}

function codeboxDecisionEvidence(result, runSummary = null, recipeSummary = null) {
  const agentResult = result.run?.agentResult || result.agentResult || result.agent_result || result.metadata?.recipe_run?.agentResult || result.metadata?.recipe_run?.run?.agentResult || {};
  const completionOutcome = result.completionOutcome || result.completion_outcome || result.metadata?.recipe_run?.completionOutcome || {};
  const runtime = result.run?.runtime || result.metadata?.recipe_run?.run?.runtime || {};
  const run = result.run || result.metadata?.recipe_run?.run || {};
  const recipeRun = recipeRunFromResult(result);
  const recipeFailedPhase = recipeSummary?.failed_phase || recipeSummary?.metadata?.failure_phase || recipeRunFailedPhase(recipeRun);
  return Object.fromEntries(Object.entries({
    selected_backend: 'codebox',
    selected_executor: 'wordpress.codebox-agent-task-executor',
    capabilities_used: PROVIDER_CAPABILITIES,
    runtime_gap_trackers: WP_CODEBOX_RUNTIME_GAP_TRACKERS,
    run_id: runSummary?.metadata?.run_id || run.runId,
    run_status: runSummary?.metadata?.run_status || run.status,
    runtime_id: runSummary?.metadata?.runtime_id || runtime.id,
    runtime_status: runSummary?.metadata?.runtime_status || runtime.status,
    heartbeat_at: run.heartbeatAt,
    cleanup_observed: (runSummary?.metadata?.runtime_status || runtime.status) === 'destroyed' ? 'runtime_destroyed' : '',
    changed_files_count: runSummary?.metadata?.changed_files_count ?? agentResult.changedFiles?.count,
    patch_bytes: runSummary?.metadata?.patch_bytes ?? agentResult.patch?.bytes,
    patch_sha256: runSummary?.metadata?.patch_sha256 || agentResult.patch?.sha256,
    no_op_reason: runSummary?.metadata?.no_op_reason || agentResult.noOpReason,
    completion_status: runSummary?.metadata?.completion_status || completionOutcome.status,
    completion_next_action: runSummary?.metadata?.completion_next_action || completionOutcome.nextAction,
    confidence: runSummary?.metadata?.confidence || completionOutcome.confidence,
    recipe_pack: recipeRun.pack || recipeRun.recipe_pack || recipeRun.recipePack,
    recipe_name: recipeRun.name || recipeRun.recipe,
    recipe_ref: recipeRun.ref,
    recipe_target_ref: recipeRun.target_ref || recipeRun.targetRef,
    recipe_failed_phase: recipeFailedPhase,
  }).filter(([, value]) => value !== undefined && value !== ''));
}

function codexProviderFromRequest(request, result = {}) {
  const configProvider = request.executor?.config?.provider;
  const resultProvider = result.task_input?.provider || result.metadata?.provider || result.provider;
  return configProvider === 'codex' || resultProvider === 'codex' ? 'codex' : '';
}

function resultDiagnostics(result = {}) {
  return [
    ...(Array.isArray(result.diagnostics) ? result.diagnostics : []),
    ...(Array.isArray(result.metadata?.diagnostics) ? result.metadata.diagnostics : []),
  ];
}

function codeboxProviderNotRegisteredCode(result = {}) {
  const candidates = [
    result.code,
    result.error_code,
    result.errorCode,
    result.metadata?.code,
    result.metadata?.error_code,
    result.metadata?.errorCode,
    ...resultDiagnostics(result).flatMap((diagnostic) => [diagnostic.class, diagnostic.code, diagnostic.kind]),
  ].filter(Boolean).map(String);
  return candidates.find((candidate) => /wp_codebox_provider_not_registered|provider_not_registered/i.test(candidate)) || '';
}

function codexProviderNotRegisteredDiagnostic(request, result = {}) {
  if (codexProviderFromRequest(request, result) !== 'codex' || !codeboxProviderNotRegisteredCode(result)) {
    return null;
  }
  const providerPluginPaths = normalizeArray(
    result.task_input?.provider_plugin_paths
      || result.metadata?.provider_plugin_paths
      || request.executor?.config?.provider_plugin_paths
  );
  return {
    class: 'codebox.codex_provider_plugin_guidance',
    message: `WP Codebox did not find a registered codex provider after loading provider plugins. ${CODEX_PROVIDER_PLUGIN_GUIDANCE}`,
    data: {
      provider: 'codex',
      provider_plugin_paths: providerPluginPaths,
      expected: 'Codex-capable ai-provider-for-openai checkout from the Codex provider branch/PR.',
      missing_provider_plugin_path: providerPluginPaths.length === 0,
      guidance: CODEX_PROVIDER_PLUGIN_GUIDANCE,
    },
  };
}

function codexDiagnosticText(result = {}) {
  const diagnostics = resultDiagnostics(result).flatMap((diagnostic) => [
    diagnostic.message,
    diagnostic.code,
    diagnostic.class,
    diagnostic.kind,
    diagnostic.data?.stderr,
    diagnostic.data?.stdout,
    diagnostic.data?.message,
  ]);
  return [
    result.summary,
    result.message,
    result.error,
    result.stderr,
    result.stdout,
    result.code,
    result.error_code,
    result.errorCode,
    result.metadata?.summary,
    result.metadata?.message,
    result.metadata?.error,
    result.metadata?.stderr,
    result.metadata?.stdout,
    ...diagnostics,
  ].filter(Boolean).map(String).join('\n');
}

function codexPhpAiClientBearerTokenDiagnostic(request, result = {}) {
  if (codexProviderFromRequest(request, result) !== 'codex') {
    return null;
  }
  const text = codexDiagnosticText(result);
  if (!/RequestAuthenticationMethod::bearerToken|bearerToken\(\).*does not exist|undefined method .*bearerToken/i.test(text)) {
    return null;
  }
  return {
    class: 'codebox.codex_php_ai_client_missing_bearer_token_auth',
    message: `Codex provider loaded, but php-ai-client does not expose bearer-token auth. ${CODEX_PHP_AI_CLIENT_GUIDANCE}`,
    data: {
      provider: 'codex',
      expected: 'php-ai-client with RequestAuthenticationMethod::bearerToken support.',
      guidance: CODEX_PHP_AI_CLIENT_GUIDANCE,
    },
  };
}

function codexPhpAiClientVendorDiagnostic(request, result = {}) {
  if (codexProviderFromRequest(request, result) !== 'codex') {
    return null;
  }
  const text = codexDiagnosticText(result);
  if (!/php-ai-client[\s\S]*(vendor\/autoload\.php|vendor(?:\/|\\\\)|composer install|Composer vendor|vendor.*missing)|vendor(?:\/|\\\\).*php-ai-client/i.test(text)) {
    return null;
  }
  return {
    class: 'codebox.codex_php_ai_client_vendor_missing',
    message: `WP Codebox could not prepare the php-ai-client runtime overlay because Composer vendor dependencies are missing. ${CODEX_PHP_AI_CLIENT_GUIDANCE}`,
    data: {
      provider: 'codex',
      expected: 'Prepared php-ai-client checkout with vendor/autoload.php present.',
      guidance: CODEX_PHP_AI_CLIENT_GUIDANCE,
    },
  };
}

function agentTaskOutcomeFromCodeboxResult(request, result = {}, options = {}) {
  assertAgentTaskRequest(request);
  const runSummary = codeboxRunSummary(result, options);
  const recipeSummary = codeboxRecipeRunSummary(result, options);
  const localStatus = normalizeStatus(result, options.exitStatus ?? 0);
  let status = runSummary?.status || recipeSummary?.status || localStatus;
  if (recipeSummary?.status && recipeSummary.status !== 'succeeded') {
    status = recipeSummary.status;
  } else if (localStatus === 'failed') {
    status = localStatus;
  }
  const failureClassification = homeboyFailureClassification(result.failure_classification || recipeSummary?.metadata?.failure_classification || runSummary?.failure_classification, status);
  const outputs = normalizeOutputs(result);
  const missingRequiredTypedArtifacts = missingRequiredTypedArtifactDiagnostic(request, outputs);
  if (status === 'succeeded' && missingRequiredTypedArtifacts) {
    status = 'failed';
  }
  const recipeRun = recipeRunFromResult(result);
  const fallbackRecipeSummary = recipeRunFailureSummary(recipeRun);
  const recipeFailedPhase = recipeSummary?.failed_phase || recipeSummary?.metadata?.failure_phase || recipeRunFailedPhase(recipeRun);
  const codexProviderDiagnostic = codexProviderNotRegisteredDiagnostic(request, result);
  const codexBearerTokenDiagnostic = codexPhpAiClientBearerTokenDiagnostic(request, result);
  const codexVendorDiagnostic = codexPhpAiClientVendorDiagnostic(request, result);
  const outcome = {
    schema: AGENT_TASK_OUTCOME_SCHEMA,
    task_id: request.task_id,
    status,
    summary: missingRequiredTypedArtifacts?.message || recipeSummary?.failure_summary || fallbackRecipeSummary || runSummary?.summary || result.summary || result.message || (status === 'succeeded' ? 'WP Codebox agent task succeeded.' : 'WP Codebox agent task failed.'),
    artifacts: normalizeArtifacts(result, runSummary, recipeSummary),
    evidence_refs: normalizeEvidenceRefs(result, runSummary, recipeSummary),
    outputs,
    diagnostics: [codexProviderDiagnostic, codexBearerTokenDiagnostic, codexVendorDiagnostic, missingRequiredTypedArtifacts, recipeSummary ? null : recipeRunFailureDiagnostic(recipeRun), ...(recipeSummary?.diagnostics || []), ...(runSummary?.diagnostics || []), ...(result.diagnostics || [])].filter(Boolean).map((diagnostic) => ({
      class: diagnostic.class || diagnostic.kind || 'codebox',
      message: diagnostic.message || String(diagnostic),
      data: sanitizePublicMetadata(diagnostic.data || {}),
    })),
    metadata: {
      provider: 'wordpress.codebox-agent-task-executor',
      codebox: sanitizePublicMetadata(result.metadata || result),
      codebox_run_result: runSummary ? sanitizePublicMetadata(runSummary) : undefined,
      codebox_recipe_run_summary: recipeSummary ? sanitizePublicMetadata(recipeSummary) : undefined,
      integration_contract: 'wp-codebox-cli/agent-task-run',
      decision_evidence: sanitizePublicMetadata(codeboxDecisionEvidence(result, runSummary, recipeSummary)),
      artifact_declarations: sanitizePublicMetadata(artifactDeclarationsMetadataFromRequest(request)),
      typed_artifacts: sanitizePublicMetadata(outputs.typed_artifacts || {}),
      sandbox_policy: sanitizePublicMetadata({
        policy: result.task_input?.policy,
        sandbox_tool_policy: result.task_input?.sandbox_tool_policy,
      }),
      recipe_failed_phase: recipeFailedPhase || undefined,
    },
  };
  if (failureClassification) {
    outcome.failure_classification = failureClassification;
  } else if (missingRequiredTypedArtifacts) {
    outcome.failure_classification = 'execution_failed';
  }
  return outcome;
}

module.exports = {
  AGENT_TASK_REQUEST_SCHEMA,
  AGENT_TASK_OUTCOME_SCHEMA,
  WP_CODEBOX_TASK_REQUEST_SCHEMA,
  PROVIDER_CAPABILITIES,
  AGENT_TASK_OUTCOME_STATUSES,
  AGENT_TASK_FAILURE_CLASSIFICATIONS,
  AGENT_TASK_REDACTED_METADATA_KEYS,
  HOMEBOY_WORDPRESS_BACKEND,
  providerContract,
  codeboxTaskRequestFromAgentTaskRequest,
  agentTaskOutcomeFromCodeboxResult,
};
