'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
  AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA,
  AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
  AGENT_TASK_FAILURE_CLASSIFICATIONS,
  AGENT_TASK_OUTCOME_SCHEMA,
  AGENT_TASK_OUTCOME_STATUSES,
  AGENT_TASK_REDACTED_METADATA_KEYS,
  AGENT_TASK_REQUEST_SCHEMA,
  agentTaskArtifactFromRef,
  agentTaskEvidenceRefFromRef,
  agentTaskProviderContractFields,
  providerDefaultsContract,
} = require('../../../runtime-agent-ci/lib/agent-task-provider-contract');
const {
  normalizeAgentTaskOutcome,
  providerFailureClassification,
} = require('./provider-outcome-normalizer');
const {
  artifactNameFromDeclaration,
  artifactPath,
  artifactRoleFromCodeboxArtifact,
  normalizeTypedArtifactEntry: normalizeCodeboxTypedArtifactEntry,
  normalizeTypedArtifacts: normalizeCodeboxTypedArtifacts,
  typedArtifactFileRefs: codeboxTypedArtifactFileRefs,
} = require('./codebox-artifact-contract');
const {
  codeboxRuntimeComponentContracts,
  codeboxRuntimeProfilePayload,
} = require('./codebox-runtime-profile');
const {
  WP_CODEBOX_BACKEND,
  WP_CODEBOX_PROVIDER_ID,
  WP_CODEBOX_PROVIDER_LABEL,
  WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES,
  WP_CODEBOX_PROVIDER_RUNTIME_INVOCATION_CONTRACT_SCHEMA,
  WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS,
  WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES,
  WP_CODEBOX_ROLE_ALIASES,
  WP_CODEBOX_TASK_REQUEST_SCHEMA,
  WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS,
  wpCodeboxProviderRuntimeInvocationContract,
  wpCodeboxProviderRuntimeOperationConfig,
  wpCodeboxProviderRuntimeOperationEntry,
} = require('./wp-codebox-adapter-contract');

const RUNTIME_MANIFEST_PATH = path.resolve(__dirname, '..', 'wp-codebox.json');
const RUNTIME_OVERLAY_CANONICAL_SHAPE = 'runtime_overlays entries must be objects. WP Codebox owns the runtime overlay schema and reports field-level validation.';
const RUNTIME_EXECUTION_DESCRIPTOR_SCHEMA = 'homeboy/runtime-execution/v1';
const PROVIDER_CAPABILITIES = runtimeProviderCapabilities();

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
  'evidence_projections',
  'ability_tools',
  'engine_data_outputs',
  'runtime_output_projections',
  'runtime_task_ability',
  'runtime_bundle_ability',
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

function assertAgentTaskRequest(request) {
  if (!request || request.schema !== AGENT_TASK_REQUEST_SCHEMA) {
    throw new Error(`Agent task request must use schema ${AGENT_TASK_REQUEST_SCHEMA}.`);
  }
  if (!request.task_id) {
    throw new Error('Agent task request requires task_id.');
  }
  const backend = request.executor?.backend;
  if (backend !== WP_CODEBOX_BACKEND) {
    throw new Error('WP Codebox executor provider only accepts executor.backend "codebox".');
  }
}

function providerContract(options = {}) {
  return {
    schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
    id: options.id || WP_CODEBOX_PROVIDER_ID,
    label: options.label || WP_CODEBOX_PROVIDER_LABEL,
    backend: WP_CODEBOX_BACKEND,
    runtime_id: options.runtimeId || options.runtime_id || runtimeManifest().id || 'wp-codebox',
    command: options.command || 'node {{runtime_path}}/scripts/agent/homeboy-codebox-agent-task-executor.cjs',
    invocation: runtimeCommandInvocation(options),
    ...agentTaskProviderContractFields(),
    secret_env_requirements: options.secretEnvRequirements || runtimeSecretEnvRequirements(),
    capabilities: normalizeArray(options.capabilities || PROVIDER_CAPABILITIES),
    workspace_materialization: {
      cwd: 'git_checkout',
    },
    runner_readiness: runtimeRunnerReadiness(options),
    workspace_tools: runtimeWorkspaceTools(options),
    component_path_defaults: runtimeComponentPathDefaults(options),
    provider_defaults: providerDefaultsContract(runtimeProviderDefaults()),
    provider_preflight: runtimeProviderPreflight(),
    provider_runtime_invocation: providerRuntimeInvocationContract(),
    role_aliases: WP_CODEBOX_ROLE_ALIASES,
    upstream_primitive_requirements: WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS,
    status: 'active',
    integration_contract: 'homeboy-wordpress-agent-task/v1',
    runtime_gap_trackers: WP_CODEBOX_RUNTIME_GAP_TRACKERS,
  };
}

function runtimeManifest() {
  return readJsonFile(RUNTIME_MANIFEST_PATH) || {};
}

function runtimeExecutorManifest() {
  return runtimeManifest().agent_task_executors?.[0] || {};
}

function runtimeCommandInvocation(options = {}) {
  return firstObject(options.invocation, runtimeExecutorManifest().invocation);
}

function runtimeProviderCapabilities() {
  return normalizeArray(runtimeExecutorManifest().capabilities);
}

function runtimeWorkspaceTools(options = {}) {
  const configured = firstObject(options.workspaceTools, options.workspace_tools, runtimeExecutorManifest().workspace_tools) || {};
  return {
    readonly: normalizeArray(configured.readonly),
    readwrite: normalizeArray(configured.readwrite),
  };
}

function runtimeComponentPathDefaults(options = {}) {
  return firstObject(options.componentPathDefaults, options.component_path_defaults, runtimeExecutorManifest().component_path_defaults) || {};
}

function runtimeComponentPathAliases(options = {}) {
  return firstObject(options.componentPathAliases, runtimeComponentPathDefaults(options).path_aliases) || {};
}

function runtimeComponentContractSlugMap(options = {}) {
  return firstObject(options.componentContractSlugMap, runtimeComponentPathDefaults(options).contract_slug_map) || {};
}

function runtimeComponentDiscovery(options = {}) {
  return firstObject(options.componentDiscovery, runtimeComponentPathDefaults(options).discovery) || {};
}

function providerRuntimeInvocationContract() {
  return wpCodeboxProviderRuntimeInvocationContract();
}

function providerRuntimeInvocationFromConfig(config = {}, inputs = {}, options = {}) {
  const requested = firstDefined(
    inputs.provider_runtime_invocation,
    inputs.providerRuntimeInvocation,
    inputs.runtime_invocation,
    inputs.runtimeInvocation,
    config.provider_runtime_invocation,
    config.providerRuntimeInvocation,
    config.runtime_invocation,
    config.runtimeInvocation,
    options.providerRuntimeInvocation,
    options.provider_runtime_invocation,
    options.runtimeInvocation,
    options.runtime_invocation,
  );
  if (requested === undefined) {
    return providerRuntimeInvocationContract();
  }

  const contract = providerRuntimeInvocationContract();
  const operationEntries = providerRuntimeOperationEntries(requested);
  if (operationEntries.length === 0) {
    return contract;
  }

  return {
    ...contract,
    operations: Object.fromEntries(operationEntries),
  };
}

function providerRuntimeOperationEntries(requested) {
  if (Array.isArray(requested)) {
    return requested
      .map((operation) => providerRuntimeOperationEntry(operation))
      .filter(Boolean);
  }
  if (!requested || typeof requested !== 'object') {
    return [];
  }
  const operations = requested.operations || requested.provider_operations || requested.providerOperations || requested.tasks || requested.abilities || requested;
  if (Array.isArray(operations)) {
    return operations
      .map((operation) => providerRuntimeOperationEntry(operation))
      .filter(Boolean);
  }
  if (!operations || typeof operations !== 'object') {
    return [];
  }
  return Object.entries(operations)
    .map(([key, operation]) => providerRuntimeOperationEntry(operation, key))
    .filter(Boolean);
}

function providerRuntimeOperationEntry(operation, fallbackKey = '') {
  return wpCodeboxProviderRuntimeOperationEntry(operation, fallbackKey);
}

function providerRuntimeOperationConfig(key, operation) {
  return wpCodeboxProviderRuntimeOperationConfig(key, operation);
}

function withoutUndefinedValues(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function runtimeProviderDefaults() {
  return firstObject(runtimeExecutorManifest().provider_defaults) || {};
}

function runtimeProviderPreflight() {
  return firstObject(runtimeExecutorManifest().provider_preflight) || {};
}

function runtimeRunnerReadiness(options = {}) {
  return normalizeArray(options.runnerReadiness || options.runner_readiness || runtimeExecutorManifest().runner_readiness);
}

function runtimeSecretEnvRequirements() {
  return normalizeArray(runtimeExecutorManifest().secret_env_requirements);
}

function codeboxTaskRequestFromAgentTaskRequest(request, options = {}) {
  assertAgentTaskRequest(request);
  const config = request.executor.config || {};
  const runtimeOptions = runtimeOptionsFromExecutorConfig(config, options);
  const inputs = request.inputs || {};
  const defaults = defaultCodeboxRuntimeConfig(request, config, inputs, runtimeOptions);
  const workspaceMaterialization = defaultWorkspaceMaterialization(defaults.workspaceRoot, request, config, inputs, runtimeOptions);
  const target = defaultWorkspaceTargetPayload(inputs.target || request.workspace || {}, workspaceMaterialization);
  const agentBundle = agentBundleConfigFromAgentTaskRequest(request, config, inputs);
  const recipe = recipeConfigFromAgentTaskRequest(request, config, inputs);
  const mounts = agentBundleMounts(agentBundle, config.runtime_mounts || config.mounts || defaults.mounts || runtimeOptions.mounts || []);
  let componentContracts = componentContractsFromAgentTaskRequest(request, config, runtimeOptions);
  let components = runtimeComponentPaths(config, { ...defaults, ...runtimeOptions, componentContracts });
  const agentBundles = firstDefined(inputs.agent_bundles, inputs.agentBundles, config.agent_bundles, config.agentBundles, runtimeOptions.agentBundles, []);
  const structuredArtifacts = firstDefined(inputs.structured_artifacts, inputs.structuredArtifacts, config.structured_artifacts, config.structuredArtifacts, runtimeOptions.structuredArtifacts, []);
  const artifactDeclarations = artifactDeclarationsFromAgentTaskRequest(request, config, inputs, runtimeOptions);
  const homeboyToolPolicy = homeboyAgentToolPolicy();
  const allowedTools = allowedToolsFromAgentTaskRequest(request, config, inputs, runtimeOptions, defaults);
  const sandboxToolPolicy = sandboxToolPolicyFromAgentTaskRequest(config, inputs, runtimeOptions, defaults, allowedTools, homeboyToolPolicy);
  const sandboxAllowedTools = allowedToolsForHomeboyToolPolicy(allowedTools, homeboyToolPolicy);
  const provider = config.provider || runtimeOptions.provider || defaults.provider || '';
  const model = request.executor.model || config.model || runtimeOptions.model || defaults.model || '';
  const homeboySecretEnvPlan = homeboyAgentTaskSecretEnvPlan();
  const plannedSecretEnv = secretEnvNamesFromPlan(homeboySecretEnvPlan);
  const agent = firstValue(config.agent, runtimeOptions.agent, '');
  const runtimeTask = runtimeTaskWithExecutionDefaults(
    inputs.runtime_task || inputs.runtimeTask || config.runtime_task || config.runtimeTask || abilityRuntimeTaskFromAgentTaskRequest(request, config, inputs) || runtimeOptions.runtimeTask,
    { provider, model, agentBundles }
  );
  const providerRuntimeInvocation = providerRuntimeInvocationFromConfig(config, inputs, runtimeOptions);
  const explicitSecretEnv = [
    ...(plannedSecretEnv.length > 0 ? plannedSecretEnv : normalizeArray(request.executor?.secret_env)),
    ...normalizeArray(config.secret_env),
    ...(plannedSecretEnv.length > 0 ? [] : normalizeArray(runtimeOptions.secretEnv)),
  ];
  const timeoutSeconds = request.limits?.task_timeout_seconds || request.limits?.taskTimeoutSeconds;
  const timeoutMs = request.limits?.timeout_ms || request.limits?.max_runtime_ms;
  const timeoutFromMs = timeoutMs ? Math.ceil(timeoutMs / 1000) : undefined;
  const runtimeOverlays = runtimeOverlaysFromConfig(config, runtimeOptions, defaults);
  const runtimeRequirements = codeboxRuntimeRequirementsFromAgentTaskRequest(config, runtimeOptions, defaults, componentContracts, runtimeOverlays);
  componentContracts = codeboxRuntimeComponentContracts({
    componentContracts: [
      ...componentContracts,
      ...normalizeArray(runtimeRequirements.component_contracts),
    ],
  });
  components = runtimeComponentPaths(config, { ...defaults, ...runtimeOptions, componentContracts });
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
    target,
    workspace_materialization: workspaceMaterialization,
    allowed_tools: sandboxAllowedTools || [],
    expected_artifacts: request.expected_artifacts || [],
    artifact_declarations: artifactDeclarations,
    policy: request.policy || {},
    context,
    recipe,
    sandbox_tool_policy: sandboxToolPolicy,
    runtime_task: runtimeTask,
    callback_data: firstDefined(inputs.callback_data, inputs.callbackData, config.callback_data, config.callbackData, runtimeOptions.callbackData),
    provider_runtime_invocation: providerRuntimeInvocation,
    ability_tools: firstDefined(inputs.ability_tools, inputs.abilityTools, config.ability_tools, config.abilityTools, runtimeOptions.abilityTools, []),
    structured_artifacts: structuredArtifacts,
    sandbox_session_id: config.sandbox_session_id || request.task_id,
    session_id: config.session_id || config.sessionId || '',
    ...(agent ? { agent } : {}),
    mode: config.mode || runtimeOptions.mode || 'sandbox',
    provider,
    model,
    provider_plugin_paths: firstNonEmptyArray(
      config.provider_plugin_paths,
      runtimeOptions.providerPluginPaths,
      defaults.providerPluginPaths,
      []
    ),
    agent_bundles: agentBundles,
    runtime_stack_mounts: config.runtime_stack_mounts || runtimeOptions.runtimeStackMounts || [],
    runtime_overlay_profiles: config.runtime_overlay_profiles || config.runtimeOverlayProfiles || runtimeOptions.runtimeOverlayProfiles || defaults.runtimeOverlayProfiles || [],
    runtime_overlays: runtimeOverlays,
    runtime_requirements: runtimeRequirements,
    runtime_env: {
      ...firstNonEmptyObject(config.runtime_env, config.runtimeEnv, config.wp_codebox_runtime_env, runtimeOptions.runtimeEnv, defaults.runtimeEnv, {}),
      ...runtimeEnvAliasesFromSourceEnv(runtimeOptions.runtimeEnvAliases),
    },
    runtime_state_mounts: firstDefined(config.runtime_state_mounts, config.runtimeStateMounts, config.wp_codebox_runtime_state_mounts, runtimeOptions.runtimeStateMounts, defaults.runtimeStateMounts, []),
    runtime_config_mounts: firstDefined(config.runtime_config_mounts, config.runtimeConfigMounts, config.wp_codebox_runtime_config_mounts, runtimeOptions.runtimeConfigMounts, defaults.runtimeConfigMounts, []),
    secret_env: explicitSecretEnv.length > 0 ? Array.from(new Set(explicitSecretEnv)) : defaults.secretEnv || [],
    // Post-agent verification gate (recipe workflow.after). Supplied as WP
    // Codebox recipe steps; a non-zero exit fails the run so the orchestrator
    // refuses to report success until the gates are green.
    verify_steps: inputs.verify_steps || config.verify_steps || runtimeOptions.verifySteps || [],
    mounts,
    workspaces: inputs.workspaces || config.workspaces || runtimeOptions.workspaces || defaults.workspaces || [],
    runtime_component_paths: components,
    component_contracts: componentContracts,
    homeboy_path: config.homeboy || config.homeboy_path || runtimeOptions.homeboy || '',
    homeboy_extensions_path: config.homeboy_extensions || config.homeboy_extensions_path || runtimeOptions.homeboyExtensions || '',
    wp_codebox_bin: firstValue(config.runtime_bin, config.wp_codebox_bin, config.wpCodeboxBin, runtimeOptions.wpCodeboxBin, defaults.wpCodeboxBin, ''),
    wp: config.runtime_wordpress_version || config.wordpress_runtime_version || config.wordpress_version || config.wp_codebox_wordpress_version || config.wpCodeboxWordpressVersion || config.wp || runtimeOptions.wpCodeboxWordpressVersion || '',
    artifacts_path: config.artifacts || config.artifacts_path || runtimeOptions.artifacts || '',
    max_turns: config.max_turns || runtimeOptions.maxTurns,
    task_timeout_seconds: config.task_timeout_seconds || timeoutSeconds || timeoutFromMs || runtimeOptions.taskTimeoutSeconds,
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

function runtimeOptionsFromExecutorConfig(config = {}, options = {}) {
  const directComponentPathDefaults = firstObject(config.component_path_defaults, config.componentPathDefaults);
  const runtimeProfile = runtimeProfileFromExecutorConfig(config, options);
  const runtimeRequirements = firstObject(config.runtime_requirements, config.runtimeRequirements) || {};
  const componentPathDefaults = directComponentPathDefaults || firstObject(options.componentPathDefaults, options.component_path_defaults, runtimeRequirements.component_path_defaults, runtimeRequirements.componentPathDefaults, runtimeProfile.component_path_defaults, runtimeProfile.componentPathDefaults);
  return {
    ...options,
    runtimeProfile,
    workspaceTools: firstObject(options.workspaceTools, options.workspace_tools, config.workspace_tools, config.workspaceTools, runtimeRequirements.workspace_tools, runtimeRequirements.workspaceTools, runtimeProfile.workspace_tools, runtimeProfile.workspaceTools),
    componentPathDefaults,
    componentPathAliases: firstObject(options.componentPathAliases, options.component_path_aliases, config.component_path_aliases, config.componentPathAliases, componentPathDefaults?.path_aliases, runtimeRequirements.component_path_aliases, runtimeRequirements.componentPathAliases),
    componentContractSlugMap: firstObject(options.componentContractSlugMap, options.component_contract_slug_map, config.component_contract_slug_map, config.componentContractSlugMap, componentPathDefaults?.contract_slug_map, runtimeRequirements.component_contract_slug_map, runtimeRequirements.componentContractSlugMap),
    componentDiscovery: firstObject(options.componentDiscovery, options.component_discovery, config.component_discovery, config.componentDiscovery, componentPathDefaults?.discovery, runtimeRequirements.component_discovery, runtimeRequirements.componentDiscovery),
    abilityRequirements: uniqueStrings([
      ...normalizeArray(options.abilityRequirements),
      ...normalizeArray(options.ability_requirements),
      ...normalizeArray(runtimeRequirements.ability_requirements),
      ...normalizeArray(runtimeRequirements.abilityRequirements),
      ...normalizeArray(runtimeProfile.ability_requirements),
      ...normalizeArray(runtimeProfile.abilityRequirements),
    ]),
    providerPluginPaths: providerPluginPathsFromRuntimeProfile(runtimeRequirements, runtimeProfile, options),
    runtimeOverlays: firstDefined(runtimeRequirements.runtime_overlays, runtimeProfile.runtime_overlays, options.runtimeOverlays),
    runtimeEnv: firstNonEmptyObject(runtimeRequirements.env, runtimeRequirements.runtime_env, runtimeProfile.env, runtimeProfile.runtime_env, options.runtimeEnv, options.runtime_env),
    runtimeEnvAliases: firstObject(runtimeRequirements.runtime_env_aliases, runtimeRequirements.runtimeEnvAliases, runtimeProfile.runtime_env_aliases, runtimeProfile.runtimeEnvAliases, options.runtimeEnvAliases, options.runtime_env_aliases),
    runtimeStateMounts: firstDefined(runtimeRequirements.runtime_state_mounts, runtimeProfile.runtime_state_mounts, options.runtimeStateMounts, options.runtime_state_mounts),
    runtimeConfigMounts: firstDefined(runtimeRequirements.runtime_config_mounts, runtimeProfile.runtime_config_mounts, options.runtimeConfigMounts, options.runtime_config_mounts),
    callbackData: firstDefined(runtimeRequirements.callback_data, runtimeRequirements.callbackData, runtimeProfile.callback_data, runtimeProfile.callbackData, options.callbackData, options.callback_data),
  };
}

function runtimeProfileFromExecutorConfig(config = {}, options = {}) {
  const runtimeProfile = firstDefined(config.runtime_profile, config.runtimeProfile, options.runtimeProfile, options.runtime_profile);
  if (runtimeProfile && typeof runtimeProfile === 'object' && !Array.isArray(runtimeProfile)) {
    return runtimeProfile;
  }
  if (typeof runtimeProfile !== 'string' || runtimeProfile.trim() === '') {
    return {};
  }
  const profiles = firstObject(config.runtime_profiles, config.runtimeProfiles, options.runtimeProfiles, options.runtime_profiles) || {};
  const namedProfile = profiles[runtimeProfile] || profiles[runtimeProfile.trim()];
  return firstObject(namedProfile) || {};
}

function artifactDeclarationsFromAgentTaskRequest(request, config = {}, inputs = {}, options = {}) {
  const declarations = firstNonEmptyArray(
    request.artifact_declarations,
    options.artifactDeclarations,
    []
  );
  if (declarations.length > 0) {
    return declarations
      .map((declaration) => wpCodeboxArtifactDeclarationFromHomeboy(declaration))
      .filter(Boolean);
  }
  return legacyArtifactDeclarationsFromAgentTaskRequest(request, config, inputs, options);
}

function legacyArtifactDeclarationsFromAgentTaskRequest(request, config = {}, inputs = {}, options = {}) {
  const declarations = [
    request.artifactDeclarations,
    inputs.artifact_declarations,
    inputs.artifactDeclarations,
    config.artifact_declarations,
    config.artifactDeclarations,
    request.artifact_outputs,
    request.artifactOutputs,
    request.output_artifacts,
    request.outputArtifacts,
    request.artifacts?.outputs,
    request.artifacts?.output_artifacts,
    request.artifacts?.outputArtifacts,
    request.outputs?.artifacts,
    request.outputs?.artifact_outputs,
    request.outputs?.artifactOutputs,
    request.outputs?.typed_artifacts,
    request.outputs?.typedArtifacts,
    inputs.artifact_outputs,
    inputs.artifactOutputs,
    inputs.output_artifacts,
    inputs.outputArtifacts,
    inputs.artifacts?.outputs,
    inputs.artifacts?.output_artifacts,
    inputs.artifacts?.outputArtifacts,
    inputs.outputs?.artifacts,
    inputs.outputs?.artifact_outputs,
    inputs.outputs?.artifactOutputs,
    inputs.outputs?.typed_artifacts,
    inputs.outputs?.typedArtifacts,
    config.artifact_outputs,
    config.artifactOutputs,
    config.output_artifacts,
    config.outputArtifacts,
    config.artifacts?.outputs,
    config.artifacts?.output_artifacts,
    config.artifacts?.outputArtifacts,
    options.artifactOutputs,
    options.outputArtifacts,
  ].flatMap(genericArtifactDeclarationEntries);
  return declarations
    .map(([name, declaration]) => wpCodeboxArtifactDeclarationFromLegacy(name, declaration))
    .filter(Boolean);
}

function genericArtifactDeclarationEntries(value) {
  if (Array.isArray(value)) {
    return value.map((declaration) => ['', declaration]);
  }
  if (!value || typeof value !== 'object') {
    return [];
  }
  return Object.entries(value);
}

function wpCodeboxArtifactDeclarationFromHomeboy(declaration) {
  return wpCodeboxArtifactDeclarationFromLegacy('', declaration);
}

function wpCodeboxArtifactDeclarationFromLegacy(defaultName, declaration) {
  if (typeof declaration === 'string') {
    return {
      schema: 'wp-codebox/artifact-declaration/v1',
      name: declaration,
      required: true,
    };
  }
  if (!declaration || typeof declaration !== 'object' || Array.isArray(declaration)) {
    return null;
  }
  const name = declaration.name || declaration.id || declaration.output || declaration.artifact || defaultName;
  if (!name || typeof name !== 'string') {
    return null;
  }
  const artifactSchema = declaration.artifact_schema
    || declaration.artifactSchema
    || declaration.content_schema
    || declaration.contentSchema
    || (declaration.schema && ![
      AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA,
      'wp-codebox/artifact-declaration/v1',
    ].includes(declaration.schema) ? declaration.schema : undefined);
  return Object.fromEntries(Object.entries({
    schema: 'wp-codebox/artifact-declaration/v1',
    name,
    type: declaration.type || declaration.kind || declaration.artifact_type || declaration.artifactType,
    artifact_schema: artifactSchema,
    path: declaration.path,
    required: declaration.required === undefined ? true : declaration.required === true,
    description: declaration.description,
    metadata: declaration.metadata,
  }).filter(([, value]) => value !== undefined));
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
    config.runtime_requirements?.runtime_overlays,
    options.runtimeProfile?.runtime_overlays,
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
  return codeboxRuntimeComponentContracts({
    profile: options.runtimeProfile,
    runtimeRequirements: firstObject(config.runtime_requirements, config.runtimeRequirements) || {},
    componentContracts: [
      ...normalizeArray(request.component_contracts),
      ...normalizeArray(config.component_contracts),
      ...normalizeArray(options.componentContracts),
    ],
  });
}

function codeboxRuntimeRequirementsFromAgentTaskRequest(config, options = {}, defaults = {}, componentContracts = [], runtimeOverlays = []) {
  const runtimeProfile = firstObject(options.runtimeProfile) || {};
  const runtimeRequirements = firstObject(config.runtime_requirements, config.runtimeRequirements) || {};
  const runtimeEnv = firstNonEmptyObject(config.runtime_env, config.runtimeEnv, config.wp_codebox_runtime_env, runtimeRequirements.env, runtimeRequirements.runtime_env, runtimeProfile.env, runtimeProfile.runtime_env, options.runtimeEnv, defaults.runtimeEnv) || {};
  const providerPluginPaths = firstNonEmptyArray(
    config.provider_plugin_paths,
    providerPluginPathsFromRuntimeProfile(runtimeRequirements, runtimeProfile, options),
    defaults.providerPluginPaths,
    []
  );
  return codeboxRuntimeProfilePayload({
    id: config.runtime_profile || config.runtimeProfile,
    profile: runtimeProfile,
    runtimeRequirements,
    componentContracts,
    runtimeOverlays,
    runtimeEnv,
    providerPluginPaths,
    runtimeStateMounts: firstDefined(config.runtime_state_mounts, config.runtimeStateMounts, config.wp_codebox_runtime_state_mounts, runtimeRequirements.runtime_state_mounts, runtimeProfile.runtime_state_mounts, options.runtimeStateMounts, defaults.runtimeStateMounts),
    runtimeConfigMounts: firstDefined(config.runtime_config_mounts, config.runtimeConfigMounts, config.wp_codebox_runtime_config_mounts, runtimeRequirements.runtime_config_mounts, runtimeProfile.runtime_config_mounts, options.runtimeConfigMounts, defaults.runtimeConfigMounts),
  });
}

function providerPluginPathsFromRuntimeProfile(runtimeRequirements = {}, runtimeProfile = {}, options = {}) {
  return uniquePaths([
    ...providerPluginPathEntries(runtimeRequirements.provider_plugins),
    ...providerPluginPathEntries(runtimeProfile.provider_plugins),
    ...normalizeArray(options.providerPluginPaths),
    ...normalizeArray(options.provider_plugin_paths),
  ]);
}

function providerPluginPathEntries(value) {
  return normalizeArray(value).flatMap((entry) => {
    if (typeof entry === 'string') {
      return [entry];
    }
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      return [];
    }
    return [entry.path, entry.source].filter(Boolean);
  });
}

function abilityRuntimeTaskFromAgentTaskRequest(request, config, inputs) {
  const genericAbilityTask = genericAbilityRuntimeTask(request, config, inputs);
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
  const input = runtimeTaskInputFromAgentTaskRequest(request, config, inputs);
  return { ability, input };
}

function genericAbilityRuntimeTask(request, config, inputs) {
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
  const input = runtimeTaskInputFromAgentTaskRequest(request, config, inputs, declared);
  return { ability, input };
}

function runtimeTaskInputFromAgentTaskRequest(request, config, inputs, declared = {}) {
  const defaultInput = firstObject(
    declared?.input_defaults,
    declared?.inputDefaults,
    inputs.ability_input_defaults,
    inputs.abilityInputDefaults,
    config.ability_input_defaults,
    config.abilityInputDefaults,
  ) || {};
  const mappedInput = runtimeMappedInputFromAgentTaskRequest(request, config, inputs, declared);
  const legacyWorkflowInputs = legacyWorkflowInputsFromAgentTaskRequest(request, config, inputs, declared);
  const explicitInput = firstObject(declared?.input, declared?.args, inputs.ability_input, inputs.abilityInput, inputs.input, config.ability_input, config.abilityInput, config.input) || {};
  return { ...defaultInput, ...mappedInput, ...legacyWorkflowInputs, ...explicitInput };
}

function runtimeMappedInputFromAgentTaskRequest(request, config, inputs, declared = {}) {
  const mappings = normalizeArray(firstDefined(
    declared?.input_mapping,
    declared?.inputMapping,
    declared?.context_mapping,
    declared?.contextMapping,
    inputs.runtime_input_mapping,
    inputs.runtimeInputMapping,
    inputs.context_mapping,
    inputs.contextMapping,
    config.runtime_input_mapping,
    config.runtimeInputMapping,
    config.context_mapping,
    config.contextMapping,
  ));
  if (mappings.length === 0) {
    return {};
  }
  const sources = runtimeInputMappingSources(request, config, inputs);
  return mappings.reduce((mapped, mapping) => {
    const entry = runtimeInputMappingEntry(mapping);
    if (!entry) {
      return mapped;
    }
    const value = valueAtPath(sources, entry.from);
    if (value === undefined) {
      if (entry.default !== undefined) {
        setValueAtPath(mapped, entry.to, entry.default);
      }
      return mapped;
    }
    setValueAtPath(mapped, entry.to, value);
    return mapped;
  }, {});
}

function runtimeInputMappingSources(request, config, inputs) {
  return {
    request,
    inputs,
    config,
    client_context: firstObject(inputs.client_context, inputs.clientContext, request.client_context, request.clientContext, config.client_context, config.clientContext) || {},
    context: firstObject(inputs.context, request.context, config.context) || {},
  };
}

function runtimeInputMappingEntry(mapping) {
  if (typeof mapping === 'string' && mapping.trim()) {
    return { from: mapping.trim(), to: pathLeaf(mapping.trim()) };
  }
  if (!mapping || typeof mapping !== 'object' || Array.isArray(mapping)) {
    return null;
  }
  const from = firstValue(mapping.from, mapping.source, mapping.path);
  const to = firstValue(mapping.to, mapping.target, mapping.name, mapping.input);
  if (!from || !to) {
    return null;
  }
  return { from, to, default: mapping.default };
}

function legacyWorkflowInputsFromAgentTaskRequest(request, config, inputs, declared = {}) {
  const legacyMerge = firstDefined(
    declared?.allow_legacy_client_context_input_merge,
    declared?.allowLegacyClientContextInputMerge,
    inputs.allow_legacy_client_context_input_merge,
    inputs.allowLegacyClientContextInputMerge,
    config.allow_legacy_client_context_input_merge,
    config.allowLegacyClientContextInputMerge,
  );
  if (legacyMerge !== true) {
    return {};
  }
  return firstObject(
    inputs.client_context?.inputs,
    inputs.clientContext?.inputs,
    request.client_context?.inputs,
    request.clientContext?.inputs,
    config.client_context?.inputs,
    config.clientContext?.inputs,
  ) || {};
}

function valueAtPath(source, fieldPath) {
  if (!fieldPath || typeof fieldPath !== 'string') {
    return undefined;
  }
  return fieldPath.split('.').filter(Boolean).reduce((value, segment) => {
    if (value === undefined || value === null || typeof value !== 'object') {
      return undefined;
    }
    return value[segment];
  }, source);
}

function setValueAtPath(target, fieldPath, value) {
  const segments = String(fieldPath || '').split('.').filter(Boolean);
  if (segments.length === 0) {
    return;
  }
  let cursor = target;
  segments.slice(0, -1).forEach((segment) => {
    if (!cursor[segment] || typeof cursor[segment] !== 'object' || Array.isArray(cursor[segment])) {
      cursor[segment] = {};
    }
    cursor = cursor[segment];
  });
  cursor[segments[segments.length - 1]] = value;
}

function pathLeaf(fieldPath) {
  const segments = String(fieldPath || '').split('.').filter(Boolean);
  return segments[segments.length - 1] || fieldPath;
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
    ...normalizeArray(options.abilityRequirements),
    ...normalizeArray(options.ability_requirements),
  ]);
  return uniqueStrings([...(defaults.allowedTools || []), ...declared]);
}

function sandboxToolPolicyFromAgentTaskRequest(config, inputs, options, defaults, allowedTools, homeboyToolPolicy) {
  const explicit = firstDefined(inputs.sandbox_tool_policy, inputs.sandboxToolPolicy, config.sandbox_tool_policy, config.sandboxToolPolicy, options.sandboxToolPolicy);
  if (explicit !== undefined) {
    return explicit;
  }
  return workspaceSandboxToolPolicyWithAllowedTools(defaults.sandboxToolPolicy, allowedToolsForHomeboyToolPolicy(allowedTools, homeboyToolPolicy));
}

function homeboyAgentToolPolicy() {
  const policy = parseJsonObject(process.env.HOMEBOY_AGENT_TOOL_POLICY_JSON);
  if (!policy || policy.schema !== 'homeboy/agent-tool-policy/v1') {
    return null;
  }
  return policy;
}

function runtimeEnvAliasesFromSourceEnv(aliases = {}) {
  if (!aliases || typeof aliases !== 'object' || Array.isArray(aliases)) {
    return {};
  }
  const env = {};
  for (const [sourceName, targetNames] of Object.entries(aliases)) {
    const value = process.env[sourceName];
    if (typeof value !== 'string' || value === '') {
      continue;
    }
    for (const targetName of normalizeArray(targetNames)) {
      if (typeof targetName === 'string' && targetName !== '') {
        env[targetName] = value;
      }
    }
  }
  return env;
}

function runtimeToolIdFromHomeboyToolId(toolId) {
  return String(toolId || '').replace(/[^A-Za-z0-9_]+/g, '_').replace(/^_+|_+$/g, '') || 'homeboy_tool';
}

function allowedToolsForHomeboyToolPolicy(allowedTools, policy) {
  if (!Array.isArray(allowedTools) || !policy || policy.schema !== 'homeboy/agent-tool-policy/v1') {
    return allowedTools;
  }
  const explicitHomeboyTools = new Map(Object.entries(policy.tools || {}).flatMap(([toolId, rule]) => {
    const id = typeof toolId === 'string' ? toolId.trim() : '';
    if (!id) {
      return [];
    }
    return [[id, rule || {}], [runtimeToolIdFromHomeboyToolId(id), rule || {}]];
  }));
  return allowedTools.filter((tool) => {
    const rule = explicitHomeboyTools.get(tool) || explicitHomeboyTools.get(runtimeToolIdFromHomeboyToolId(tool));
    return !rule || (rule.execution_location || policy.default_location) === 'runner';
  });
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
  const existingRuntimeToolIds = new Set((basePolicy.tools || []).flatMap((tool) => [tool?.id, tool?.runtime_tool_id]).filter(Boolean));
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
  const runtimeComponents = config.runtime_components && typeof config.runtime_components === 'object'
    ? config.runtime_components
    : {};
  const explicit = config.runtime_component_paths && typeof config.runtime_component_paths === 'object'
    ? config.runtime_component_paths
    : {};
  const contractPaths = runtimeComponentPathsFromContracts(config.component_contracts || options.componentContracts || [], options);
  const aliases = runtimeComponentPathAliases(options);
  const resolved = {
    ...contractPaths,
    agents_api: config.agents_api || config.agents_api_path || options.agentsApi,
    agent_runtime: config.agent_runtime || config.data_machine || config.data_machine_path || options.agentRuntime,
    agent_runtime_tools: config.agent_runtime_tools || config.data_machine_code || config.data_machine_code_path || options.agentRuntimeTools,
    ...explicit,
    runtime: explicit.runtime || runtimeComponents.runtime,
  };

  for (const [key, candidates] of Object.entries(aliases)) {
    if (resolved[key]) {
      continue;
    }
    resolved[key] = firstValue(...normalizeArray(candidates).map((candidate) => componentPathCandidateValue(candidate, {
      config,
      runtimeComponents,
      explicit,
      contractPaths,
      options,
    })));
  }

  return Object.fromEntries(Object.entries(resolved).filter(([, value]) => value !== undefined && value !== ''));
}

function componentPathCandidateValue(candidate, sources) {
  if (typeof candidate !== 'string') {
    return '';
  }
  const [scope, key] = candidate.split(':');
  if (!key) {
    return sources.explicit[candidate]
      || sources.runtimeComponents[candidate]
      || sources.contractPaths[candidate]
      || sources.config[candidate]
      || sources.config[`${candidate}_path`]
      || sources.options[candidate]
      || '';
  }
  if (scope === 'explicit') {
    return sources.explicit[key] || '';
  }
  if (scope === 'runtime_component') {
    return sources.runtimeComponents[key] || '';
  }
  if (scope === 'contract') {
    return sources.contractPaths[key] || '';
  }
  if (scope === 'config') {
    return sources.config[key] || '';
  }
  if (scope === 'config_path') {
    return sources.config[`${key}_path`] || '';
  }
  if (scope === 'option') {
    return sources.options[key] || '';
  }
  return '';
}

function runtimeComponentPathsFromContracts(contracts, options = {}) {
  if (!Array.isArray(contracts)) {
    return {};
  }
  const slugToKey = new Map(Object.entries(runtimeComponentContractSlugMap(options)));
  return Object.fromEntries(contracts
    .map((contract) => [slugToKey.get(contract?.slug), contract?.path || contract?.source])
    .filter(([key, value]) => key && value));
}

function defaultCodeboxRuntimeConfig(request, config, inputs, options = {}) {
  const settings = firstObject(options.settings, parseJsonObject(process.env.HOMEBOY_SETTINGS_JSON)) || {};
  const discovery = runtimeComponentDiscovery(options);
  const workspaceRoot = resolveWorkspaceRoot(request, config, inputs, settings, options);
  const workspaceBase = workspaceRoot ? path.dirname(workspaceRoot) : process.cwd();
  const agentRuntimePath = firstExistingPath(
    options.agentRuntime,
    ...componentDiscoveryCandidates('agent_runtime', discovery, settings, workspaceBase),
  );
  const agentRuntimeToolsPath = firstExistingPath(
    options.agentRuntimeTools,
    ...componentDiscoveryCandidates('agent_runtime_tools', discovery, settings, workspaceBase),
  );
  const providerPluginPath = firstExistingPath(
    settings.wp_codebox_provider_plugin_path,
    process.env.HOMEBOY_WP_CODEBOX_PROVIDER_PLUGIN_PATH,
  );
  const providerDefaults = runtimeProviderDefaultsFromSettings(settings);
  const provider = config.provider || options.provider || defaultProvider(settings);
  const providerConfig = providerConfigFor(provider, settings, providerDefaults);
  const model = config.model || options.model || defaultModelForProvider(provider, settings, providerConfig);
  const agentsApiPath = firstExistingPath(
    options.agentsApi,
    ...componentDiscoveryCandidates('agents_api', discovery, settings, workspaceBase, { agent_runtime: agentRuntimePath }),
  );
  const phpAiClientPath = defaultPhpAiClientPath(settings, options);

  return {
    agentsApi: agentsApiPath,
    agentRuntime: agentRuntimePath,
    agentRuntimeTools: agentRuntimeToolsPath,
    legacyRuntime: agentRuntimePath,
    legacyRuntimeTools: agentRuntimeToolsPath,
    providerPluginPaths: defaultProviderPluginPaths(provider, config, options, settings, providerConfig, providerPluginPath),
    provider,
    model,
    secretEnv: defaultSecretEnv(config, options, settings, providerConfig),
    wpCodeboxBin: firstValue(settings.wp_codebox_bin, settings.wpCodeboxBin, process.env.HOMEBOY_WP_CODEBOX_BIN, ''),
    runtimeOverlayProfiles: defaultRuntimeOverlayProfiles(settings),
    runtimeOverlays: defaultRuntimeOverlays(settings, phpAiClientPath),
    runtimeEnv: defaultRuntimeEnv(settings),
    runtimeStateMounts: defaultRuntimeStateMounts(settings),
    runtimeConfigMounts: defaultRuntimeConfigMounts(settings),
    workspaceRoot,
    mounts: defaultWorkspaceMounts(workspaceRoot, request, config, inputs, options),
    workspaces: defaultWorkspaces(config, inputs, options),
    allowedTools: defaultWorkspaceAllowedTools(workspaceRoot, workspaceMode(request, config, inputs), options),
    sandboxToolPolicy: defaultWorkspaceSandboxToolPolicy(workspaceRoot, workspaceMode(request, config, inputs), options),
  };
}

function defaultProviderPluginPaths(provider, config, options, settings, providerConfig, fallbackProviderPluginPath) {
  return uniquePaths(firstProviderPathArray(
    config.provider_plugin_paths,
    options.providerPluginPaths,
    providerPathsFor(settings.wp_codebox_provider_plugin_paths, provider),
    providerPathsFor(settings.provider_plugin_paths, provider),
    providerConfig.provider_plugin_paths,
    fallbackProviderPluginPath ? [fallbackProviderPluginPath] : undefined,
    []
  ));
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

function withoutEmptyObjectValues(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => {
    if (Array.isArray(entry)) {
      return entry.length > 0;
    }
    if (entry && typeof entry === 'object') {
      return Object.keys(entry).length > 0;
    }
    return entry !== undefined && entry !== '';
  }));
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

function homeboyAgentTaskSecretEnvPlan() {
  return parseJsonObject(process.env.HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON);
}

function secretEnvNamesFromPlan(plan) {
  if (!plan || plan.schema !== 'homeboy/secret-env-plan/v1') {
    return [];
  }
  return uniquePaths([
    ...normalizeArray(plan.secret_env_names),
    ...Object.values(plan.env_name_mapping || {}).flatMap(normalizeArray),
  ]);
}

function normalizeArray(value) {
  if (Array.isArray(value)) {
    return value.filter(Boolean);
  }
  return value ? [value] : [];
}

function componentDiscoveryCandidates(componentKey, discovery, settings, workspaceBase, resolved = {}) {
  return normalizeArray(discovery[componentKey]).flatMap((entry) => {
    if (typeof entry === 'string') {
      return [entry];
    }
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      return [];
    }
    if (entry.settings) {
      return normalizeArray(entry.settings).map((key) => settings[key]);
    }
    if (entry.env) {
      return normalizeArray(entry.env).map((key) => process.env[key]);
    }
    if (entry.active_plugin) {
      return [activeSitePluginPath(entry.active_plugin)];
    }
    if (entry.sibling) {
      return [siblingPath(workspaceBase, entry.sibling)];
    }
    if (entry.bundled_provider) {
      return bundledProviderPaths(resolved[entry.bundled_provider], entry.paths);
    }
    return [];
  });
}

function firstNonEmptyArray(...values) {
  for (const value of values) {
    const normalized = normalizeArray(value);
    if (normalized.length > 0) {
      return normalized;
    }
  }
  return [];
}

function firstExistingPath(...candidates) {
  for (const candidate of candidates.flatMap((value) => normalizeArray(value))) {
    if (candidate && fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return '';
}

function uniquePaths(paths) {
  return Array.from(new Set(paths.filter(Boolean)));
}

function siblingPath(base, name) {
  return base && name ? path.join(base, name) : '';
}

function activeSitePluginPath(slug) {
  const candidate = path.join(process.cwd(), 'wp-content', 'plugins', slug);
  return fs.existsSync(candidate) ? candidate : '';
}

function bundledProviderPaths(providerPath, childPaths) {
  return providerPath ? normalizeArray(childPaths).map((childPath) => path.join(providerPath, childPath)) : [];
}

function defaultSecretEnv(config, options, settings, providerConfig) {
  return uniquePaths(firstProviderPathArray(
    config.secret_env,
    options.secretEnv,
    settings.wp_codebox_secret_env,
    settings.secret_env,
    providerConfig.secret_env,
    []
  ));
}

function defaultProvider(settings) {
  const explicit = settings.wp_codebox_provider || settings.provider || process.env.HOMEBOY_WP_CODEBOX_PROVIDER;
  if (explicit) {
    return explicit;
  }
  return settings.wp_codebox_default_provider || settings.default_provider || '';
}

function defaultModelForProvider(provider, settings, providerConfig) {
  const explicit = settings.wp_codebox_model || settings.model || process.env.HOMEBOY_WP_CODEBOX_MODEL;
  if (explicit) {
    return explicit;
  }
  return providerConfig.model || '';
}

function runtimeProviderDefaultsFromSettings(settings = {}) {
  return {
    ...runtimeProviderDefaults(),
    ...(firstObject(settings.wp_codebox_provider_defaults, settings.provider_defaults) || {}),
  };
}

function providerConfigFor(provider, settings = {}, providerDefaults = {}) {
  if (!provider) {
    return {};
  }
  return {
    ...(firstObject(providerDefaults[provider]) || {}),
    ...(firstObject(settings.wp_codebox_providers?.[provider], settings.providers?.[provider]) || {}),
  };
}

function providerPathsFor(value, provider) {
  if (!value || Array.isArray(value) || typeof value !== 'object') {
    return normalizeArray(value);
  }
  return normalizeArray(value[provider]);
}

function firstProviderPathArray(...values) {
  for (const value of values) {
    const normalized = normalizeArray(value);
    if (normalized.length > 0) {
      return normalized;
    }
  }
  return [];
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

function defaultWorkspaceToolIds(workspaceRoot, workspaceModeValue, options = {}) {
  if (!workspaceRoot) {
    return [];
  }
  const configuredTools = runtimeWorkspaceTools(options);
  if (workspaceModeValue === 'readwrite') {
    return uniqueStrings([...configuredTools.readonly, ...configuredTools.readwrite]);
  }
  return configuredTools.readonly;
}

function defaultWorkspaceAllowedTools(workspaceRoot, workspaceModeValue, options = {}) {
  return defaultWorkspaceToolIds(workspaceRoot, workspaceModeValue, options);
}

function defaultWorkspaceSandboxToolPolicy(workspaceRoot, workspaceModeValue, options = {}) {
  if (!workspaceRoot) {
    return undefined;
  }
  return {
    schema: 'wp-codebox/sandbox-tool-policy/v1',
    version: 1,
    tools: defaultWorkspaceToolIds(workspaceRoot, workspaceModeValue, options).map((tool) => ({
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
  const workspaceContext = genericWorkspaceContext(request, config, inputs, options);
  const candidates = [
    options.workspaceRoot,
    workspaceContext.cwd,
    inputs.target?.root,
    inputs.target?.path,
    request.workspace?.root,
    request.workspace?.path,
    config.workspace_root,
    config.workspaceRoot,
    config.cwd,
    settings.wp_codebox_workspace_root,
    process.env.HOMEBOY_COMPONENT_PATH,
  ];
  return firstExistingPath(...candidates) || '';
}

function defaultWorkspaceMaterialization(workspaceRoot, request, config, inputs, options) {
  const context = genericWorkspaceContext(request, config, inputs, options);
  const repo = firstValue(
    context.repo,
    request.workspace?.materialization?.repo,
    request.workspace?.repo,
    request.workspace?.slug,
    config.repo,
    config.repository,
  );
  const cwd = firstValue(context.cwd, workspaceRoot);
  return Object.fromEntries(Object.entries({
    ...request.workspace?.materialization,
    repo,
    cwd,
    root: workspaceRoot || undefined,
  }).filter(([, value]) => value !== undefined && value !== ''));
}

function defaultWorkspaceTargetPayload(target, workspaceMaterialization) {
  if (!workspaceMaterialization || Object.keys(workspaceMaterialization).length === 0) {
    return target;
  }
  return {
    ...target,
    materialization: {
      ...(target.materialization || {}),
      ...workspaceMaterialization,
    },
  };
}

function genericWorkspaceContext(request, config, inputs, options) {
  const clientContext = firstObject(
    inputs.client_context,
    inputs.clientContext,
    request.client_context,
    request.clientContext,
    config.client_context,
    config.clientContext,
    options.clientContext,
  ) || {};
  const context = firstObject(inputs.context, request.context, config.context, options.context) || {};
  const workspace = firstObject(request.workspace, inputs.workspace, config.workspace, options.workspace) || {};
  const clientInputs = firstObject(clientContext.inputs) || {};

  return {
    cwd: firstValue(
      options.cwd,
      inputs.cwd,
      inputs.workspace_cwd,
      inputs.workspaceCwd,
      request.cwd,
      request.working_directory,
      request.workingDirectory,
      context.cwd,
      clientContext.cwd,
      clientInputs.cwd,
      workspace.cwd,
      config.cwd,
    ),
    repo: firstValue(
      options.repo,
      inputs.repo,
      inputs.repository,
      request.repo,
      request.repository,
      context.repo,
      clientContext.repo,
      clientInputs.repo,
      workspace.repo,
      config.repo,
      config.repository,
    ),
  };
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

function firstNonEmptyObject(...candidates) {
  return candidates.find((candidate) => candidate && typeof candidate === 'object' && !Array.isArray(candidate) && Object.keys(candidate).length > 0) || null;
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

function normalizeStatus(result, exitStatus = 0) {
  if (recipeRunFailedPhase(recipeRunFromResult(result))) {
    return 'failed';
  }
  if (result?.outputs && typeof result.outputs === 'object' && result.outputs.success === false) {
    return 'failed';
  }
  if (agentRuntimeFailure(result)) {
    return 'failed';
  }
  if (result?.status === 'failed' || result?.status === 'provider_error' || result?.status === 'timeout' || result?.status === 'unable_to_remediate') {
    return result.status;
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
  if ((exitStatus ?? 0) !== 0) {
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
  return result?.success === true ? 'succeeded' : 'failed';
}

function agentRuntimeResultCandidates(result) {
  const workload = agentRuntimeWorkload(result) || {};
  const scenarios = Array.isArray(workload.scenarios) ? workload.scenarios : [];
  return [
    result?.outputs?.agent_runtime?.result,
    result?.outputs?.agent_runtime?.workload,
    result?.outputs?.agent_runtime,
    result?.raw?.agent_runtime,
    result?.raw?.agent_runtime?.result,
    result?.raw?.agent_runtime?.workload,
    result?.metadata?.agent_runtime,
    result?.metadata?.agent_runtime?.result,
    result?.metadata?.agent_runtime?.workload,
    result?.run?.agentResult,
    result?.agentResult,
    result?.agent_result,
    result?.outputs,
    workload,
    workload.outputs,
    ...scenarios,
    ...scenarios.map((scenario) => scenario?.metadata),
    ...scenarios.map((scenario) => scenario?.outputs),
  ].filter((candidate) => candidate && typeof candidate === 'object' && !Array.isArray(candidate));
}

function agentRuntimeWorkload(result) {
  return firstObject(
    result?.outputs?.agent_runtime?.result,
    result?.outputs?.agent_runtime?.workload,
    result?.raw?.agent_runtime?.result,
    result?.raw?.agent_runtime?.workload,
    result?.metadata?.agent_runtime?.result,
    result?.metadata?.agent_runtime?.workload,
    result?.run?.agentResult,
    result?.agentResult,
    result?.agent_result,
  );
}

function agentRuntimeFailure(result) {
  return agentRuntimeResultCandidates(result).find(agentRuntimeCandidateFailed) || null;
}

function agentRuntimeCandidateFailed(candidate) {
  const terminalStatus = String(candidate.terminal_status || candidate.terminalStatus || '').toLowerCase();
  const status = String(candidate.status || candidate.outcome || '').toLowerCase();
  const completionStatus = String(candidate.completion_outcome?.status || candidate.completionOutcome?.status || '').toLowerCase();
  return candidate.success === false
    || candidate.completion_outcome?.success === false
    || candidate.completionOutcome?.success === false
    || terminalStatus === 'failed'
    || terminalStatus.startsWith('failed ')
    || status === 'failed'
    || completionStatus === 'failed'
    || Boolean(candidate.error_reason || candidate.errorReason || candidate.error_step_id || candidate.errorStepId);
}

function agentRuntimeFailureReason(runtimeFailure) {
  if (!runtimeFailure) {
    return '';
  }
  const terminalStatus = String(runtimeFailure.terminal_status || runtimeFailure.terminalStatus || '');
  return firstValue(
    runtimeFailure.error_reason,
    runtimeFailure.errorReason,
    runtimeFailure.reason,
    runtimeFailure.completion_outcome?.reason,
    runtimeFailure.completionOutcome?.reason,
    terminalStatus.startsWith('failed - ') ? terminalStatus.slice('failed - '.length).trim() : '',
    runtimeFailure.status,
  );
}

function agentRuntimeFailureDiagnostic(result) {
  const runtimeFailure = agentRuntimeFailure(result);
  if (!runtimeFailure) {
    return null;
  }
  const reason = agentRuntimeFailureReason(runtimeFailure);
  const message = runtimeFailure.error_message
    || runtimeFailure.errorMessage
    || runtimeFailure.message
    || runtimeFailure.summary
    || (reason ? `Embedded agent runtime failed: ${reason}.` : 'Embedded agent runtime failed.');
  return {
    class: 'agent_runtime.failed',
    message,
    data: sanitizePublicMetadata({
      reason,
      status: runtimeFailure.status,
      terminal_status: runtimeFailure.terminal_status || runtimeFailure.terminalStatus,
      error_reason: runtimeFailure.error_reason || runtimeFailure.errorReason,
      error_step_id: runtimeFailure.error_step_id || runtimeFailure.errorStepId,
    }),
  };
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
  return providerFailureClassification(classification, status);
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
  return normalizeCodeboxTypedArtifactEntry(name, artifact, { sanitize: sanitizePublicMetadata });
}

function normalizeTypedArtifacts(value) {
  return normalizeCodeboxTypedArtifacts(value, { sanitize: sanitizePublicMetadata });
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
    result.metadata?.agent_runtime?.result?.outputs?.outputs?.typed_artifacts,
    result.metadata?.agent_runtime?.result?.outputs?.outputs?.typedArtifacts,
    workload.typed_artifacts,
    workload.typedArtifacts,
    workload.outputs?.typed_artifacts,
    workload.outputs?.typedArtifacts,
    workload.outputs?.outputs?.typed_artifacts,
    workload.outputs?.outputs?.typedArtifacts,
    ...scenarios.map((scenario) => scenario?.typed_artifacts),
    ...scenarios.map((scenario) => scenario?.typedArtifacts),
    ...scenarios.map((scenario) => scenario?.outputs?.typed_artifacts),
    ...scenarios.map((scenario) => scenario?.outputs?.typedArtifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.outputs?.typed_artifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.outputs?.typedArtifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.typed_artifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.typedArtifacts),
  ];
  return Object.assign({}, ...candidates.map(normalizeTypedArtifacts));
}

function codeboxAgentResultFromResult(result) {
  return result.run?.agentResult || result.agentResult || result.agent_result || result.metadata?.recipe_run?.agentResult || {};
}

function codeboxBundleDirectoryFromResult(result) {
  const completionOutcome = result.completionOutcome || result.completion_outcome || {};
  const agentResult = codeboxAgentResultFromResult(result);
  return agentResult.artifacts?.directory
    || completionOutcome?.provenance?.artifactDirectory
    || result.session?.artifacts?.path
    || result.session?.artifacts?.directory
    || (typeof result.artifacts === 'string' ? result.artifacts : '')
    || '';
}

function firstTranscriptArtifactRefFromResult(result) {
  const agentResult = codeboxAgentResultFromResult(result);
  const directPath = artifactPath(codeboxBundleDirectoryFromResult(result), agentResult.transcript?.artifact || '');
  if (directPath) {
    return {
      kind: 'codebox-transcript',
      path: directPath,
      mime: 'application/json',
      metadata: agentResult.transcript || {},
      schema: agentResult.transcript?.schema,
    };
  }

  const candidates = [
    ...(Array.isArray(result.artifacts) ? result.artifacts : Object.values(result.artifacts || {}).filter((value) => value && typeof value === 'object')),
    ...agentRuntimeBundleArtifacts(result),
  ];
  const transcriptArtifact = candidates
    .map((artifact) => artifactFromCodeboxArtifact(artifact))
    .find((artifact) => artifact?.path && /transcript|conversation|messages/i.test(`${artifact.kind || ''} ${artifact.name || ''} ${artifact.path || ''}`));
  if (transcriptArtifact) {
    return {
      kind: transcriptArtifact.kind || 'codebox-transcript',
      path: transcriptArtifact.path,
      url: transcriptArtifact.url,
      mime: transcriptArtifact.mime || 'application/json',
      metadata: transcriptArtifact.metadata || {},
      schema: transcriptArtifact.metadata?.schema || transcriptArtifact.metadata?.artifact_schema,
    };
  }

  const bundleDirectory = codeboxBundleDirectoryFromResult(result);
  const fallbackPath = artifactPath(bundleDirectory, 'files/transcript.json');
  if (fallbackPath && fs.existsSync(fallbackPath)) {
    return {
      kind: 'codebox-transcript',
      path: fallbackPath,
      mime: 'application/json',
      metadata: { source: 'codebox_artifact_directory' },
    };
  }

  return null;
}

function isTranscriptArtifactDeclaration(declaration) {
  const name = typedArtifactNameFromDeclaration(declaration);
  const type = declaration?.type || declaration?.kind || declaration?.artifact_type || declaration?.artifactType || '';
  const schema = declaration?.artifact_schema || declaration?.artifactSchema || declaration?.schema || '';
  return /transcript|conversation|messages/i.test(`${name} ${type} ${schema}`);
}

function transcriptTypedArtifactsFromCodeboxResult(request, result, existingTypedArtifacts = {}) {
  if (!request) {
    return {};
  }
  const requiredTranscriptArtifacts = requiredArtifactDeclarationsFromRequest(request).filter(isTranscriptArtifactDeclaration);
  if (requiredTranscriptArtifacts.length === 0) {
    return {};
  }
  const transcriptRef = firstTranscriptArtifactRefFromResult(result);
  if (!transcriptRef?.path && !transcriptRef?.url) {
    return {};
  }
  return Object.fromEntries(requiredTranscriptArtifacts
    .map((declaration) => typedArtifactNameFromDeclaration(declaration))
    .filter((name) => name && !existingTypedArtifacts[name])
    .map((name) => [name, normalizeTypedArtifactEntry(name, {
      name,
      type: 'transcript',
      artifact_schema: transcriptRef.schema || 'wp-codebox/agent-transcript/v1',
      file_refs: [{ kind: transcriptRef.kind || 'codebox-transcript', path: transcriptRef.path, url: transcriptRef.url, mime: transcriptRef.mime || 'application/json' }],
      metadata: transcriptRef.metadata || {},
    })])
    .filter(([, artifact]) => artifact));
}

function typedArtifactProjectionFromOutputPath(outputPath, value, typedArtifacts) {
  const match = String(outputPath || '').match(/(?:^|\.)typed_?artifacts\.([^.]+)\.payload$/i);
  if (!match) {
    return null;
  }
  const artifactName = match[1];
  const existing = typedArtifacts[artifactName] || {};
  return normalizeTypedArtifactEntry(artifactName, {
    ...existing,
    name: existing.name || artifactName,
    payload: value,
  });
}

function typedArtifactFileRefs(typedArtifact) {
  const refs = codeboxTypedArtifactFileRefs(typedArtifact);
  const directRefs = [typedArtifact.path, typedArtifact.url, typedArtifact.file, typedArtifact.directory].filter(Boolean);
  return [
    ...directRefs.map((ref) => ({ path: ref })),
    ...refs,
  ];
}

function typedArtifactNameFromDeclaration(declaration) {
  return artifactNameFromDeclaration(declaration);
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
  const normalized = agentTaskArtifactFromRef(
    {
      ...artifact,
      id: artifact.id || artifact.sha256 || artifact.path || artifact.url || `codebox-artifact-${index + 1}`,
      kind: artifact.kind || artifact.type || 'codebox_artifact',
    },
    index,
    sanitizePublicMetadata
  );
  return normalizeCodeboxArtifactOutcome(normalized, artifact);
}

function normalizeCodeboxArtifactOutcome(artifact, rawArtifact = {}) {
  if (!artifact) {
    return artifact;
  }
  const nativeKind = rawArtifact.kind || rawArtifact.type || artifact.kind || '';
  const role = artifactRoleFromCodeboxArtifact({ ...artifact, kind: nativeKind }, WP_CODEBOX_ROLE_ALIASES);
  const metadata = artifact.metadata && typeof artifact.metadata === 'object' && !Array.isArray(artifact.metadata)
    ? artifact.metadata
    : {};
  return {
    ...artifact,
    role,
    metadata: sanitizePublicMetadata({
      ...metadata,
      wp_codebox: {
        id: rawArtifact.id || artifact.id,
        kind: nativeKind,
        name: rawArtifact.name || artifact.name,
        raw: rawArtifact,
      },
    }),
  };
}

function pathValue(source, dottedPath) {
  return String(dottedPath || '').split('.').filter(Boolean).reduce((value, key) => (value && typeof value === 'object' ? value[key] : undefined), source);
}

function firstPlainObject(...candidates) {
  return candidates.find((candidate) => candidate && typeof candidate === 'object' && !Array.isArray(candidate));
}

function normalizeOutputs(result, request = null) {
  const workload = agentRuntimeWorkload(result) || {};
  const typedArtifacts = typedArtifactsFromResult(result);
  Object.assign(typedArtifacts, transcriptTypedArtifactsFromCodeboxResult(request, result, typedArtifacts));
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
  const configuredOutputs = firstPlainObject(bundle.runtime_output_projections, bundle.runtimeOutputProjections, bundle.engine_data_outputs, bundle.engineDataOutputs) || {};
  if (Object.keys(configuredOutputs).length === 0 && workload.outputs && typeof workload.outputs === 'object' && !Array.isArray(workload.outputs)) {
    return sanitizePublicMetadata(appendTypedArtifacts(workload.outputs));
  }
  if (Object.keys(configuredOutputs).length === 0 && result.outputs && typeof result.outputs === 'object' && !Array.isArray(result.outputs)) {
    return sanitizePublicMetadata(appendTypedArtifacts(result.outputs));
  }
  const scenarios = Array.isArray(workload.scenarios) ? workload.scenarios : [];
  const configuredOutputSources = [
    ...scenarios,
    ...scenarios.map((scenario) => scenario?.metadata),
    ...scenarios.map((scenario) => scenario?.outputs),
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
        const typedArtifact = typedArtifactProjectionFromOutputPath(outputPath, value, typedArtifacts);
        if (typedArtifact) {
          typedArtifacts[typedArtifact.name] = typedArtifact;
        }
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
      appendUniqueEvidenceRef(refs, agentTaskEvidenceRefFromRef(ref, 'codebox_evidence'));
    }
    return refs;
  }
  const evidenceRefs = result?.evidence_refs || result?.evidence || [];
  return evidenceRefs.map((ref) => agentTaskEvidenceRefFromRef(ref, 'codebox_evidence')).filter((ref) => ref.uri);
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
    agent_runtime_failure_reason: agentRuntimeFailureReason(agentRuntimeFailure(result)),
  }).filter(([, value]) => value !== undefined && value !== ''));
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

function providerFromRequest(request, result = {}) {
  return request.executor?.config?.provider
    || result.task_input?.provider
    || result.metadata?.provider
    || result.provider
    || '';
}

function providerNotRegisteredDiagnostic(request, result = {}) {
  const provider = providerFromRequest(request, result);
  if (!provider || !codeboxProviderNotRegisteredCode(result)) {
    return null;
  }
  const providerPluginPaths = normalizeArray(
    result.task_input?.provider_plugin_paths
      || result.metadata?.provider_plugin_paths
      || request.executor?.config?.provider_plugin_paths
  );
  return {
    class: 'codebox.provider_not_registered',
    message: `WP Codebox did not find a registered ${provider} provider after loading provider plugins.`,
    data: {
      provider,
      provider_plugin_paths: providerPluginPaths,
      missing_provider_plugin_path: providerPluginPaths.length === 0,
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
  const outputs = normalizeOutputs(result, request);
  const missingRequiredTypedArtifacts = missingRequiredTypedArtifactDiagnostic(request, outputs);
  if (status === 'succeeded' && missingRequiredTypedArtifacts) {
    status = 'failed';
  }
  const recipeRun = recipeRunFromResult(result);
  const fallbackRecipeSummary = recipeRunFailureSummary(recipeRun);
  const recipeFailedPhase = recipeSummary?.failed_phase || recipeSummary?.metadata?.failure_phase || recipeRunFailedPhase(recipeRun);
  const runtimeFailureDiagnostic = agentRuntimeFailureDiagnostic(result);
  const providerDiagnostic = providerNotRegisteredDiagnostic(request, result);
  const outcome = normalizeAgentTaskOutcome(request, result, {
    schema: AGENT_TASK_OUTCOME_SCHEMA,
    provider: 'wordpress.codebox-agent-task-executor',
    providerLabel: 'WP Codebox agent',
    integrationContract: 'wp-codebox-cli/agent-task-run',
    status,
    summary: missingRequiredTypedArtifacts?.message || runtimeFailureDiagnostic?.message || recipeSummary?.failure_summary || fallbackRecipeSummary || runSummary?.summary || result.summary || result.message || (status === 'succeeded' ? 'WP Codebox agent task succeeded.' : 'WP Codebox agent task failed.'),
    artifacts: normalizeArtifacts(result, runSummary, recipeSummary),
    evidenceRefs: normalizeEvidenceRefs(result, runSummary, recipeSummary),
    outputs,
    diagnostics: [providerDiagnostic, missingRequiredTypedArtifacts, runtimeFailureDiagnostic, recipeSummary ? null : recipeRunFailureDiagnostic(recipeRun), ...(recipeSummary?.diagnostics || []), ...(runSummary?.diagnostics || []), ...(result.diagnostics || [])].filter(Boolean).map((diagnostic) => ({
      class: diagnostic.class || diagnostic.kind || 'codebox',
      message: diagnostic.message || String(diagnostic),
      data: sanitizePublicMetadata(diagnostic.data || {}),
    })),
    metadata: {
      codebox: sanitizePublicMetadata(result.metadata || result),
      codebox_run_result: runSummary ? sanitizePublicMetadata(runSummary) : undefined,
      codebox_recipe_run_summary: recipeSummary ? sanitizePublicMetadata(recipeSummary) : undefined,
      decision_evidence: sanitizePublicMetadata(codeboxDecisionEvidence(result, runSummary, recipeSummary)),
      artifact_declarations: sanitizePublicMetadata(artifactDeclarationsMetadataFromRequest(request)),
      typed_artifacts: sanitizePublicMetadata(outputs.typed_artifacts || {}),
      sandbox_policy: sanitizePublicMetadata({
        policy: result.task_input?.policy,
        sandbox_tool_policy: result.task_input?.sandbox_tool_policy,
      }),
      recipe_failed_phase: recipeFailedPhase || undefined,
    },
    failureClassification,
  });
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
  WP_CODEBOX_BACKEND,
  WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES,
  WP_CODEBOX_PROVIDER_RUNTIME_INVOCATION_CONTRACT_SCHEMA,
  WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS,
  WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES,
  RUNTIME_EXECUTION_DESCRIPTOR_SCHEMA,
  providerContract,
  providerRuntimeInvocationContract,
  codeboxTaskRequestFromAgentTaskRequest,
  agentTaskOutcomeFromCodeboxResult,
};
