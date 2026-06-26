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
} = require('../../../agent-task-contracts/agent-task-provider-contract');
const {
  normalizeAgentTaskOutcome,
  providerFailureClassification,
} = require('../../../runtime-agent-ci/lib/agent-task-outcome-normalizer');
const {
  artifactResultEnvelopeFromCodeboxResult,
  artifactNameFromDeclaration,
  normalizeCodeboxArtifactDeclaration,
  normalizeCodeboxArtifactOutcome: normalizeCodeboxArtifactOutcomeContract,
  normalizeTypedArtifactEntry: normalizeCodeboxTypedArtifactEntry,
  normalizeTypedArtifacts: normalizeCodeboxTypedArtifacts,
  typedArtifactsFromCodeboxResult,
  typedArtifactFileRefs: codeboxTypedArtifactFileRefs,
} = require('./codebox-artifact-contract');
const {
  codeboxPublicResultEnvelope,
  privateCodeboxRuntimeResultShapeNames,
  publicEnvelopeBoundaryDiagnostic,
} = require('./codebox-result-boundary');
const {
  codeboxRuntimeComponentContracts,
  codeboxRuntimeProfilePayload,
} = require('./codebox-runtime-profile');
const {
  WP_CODEBOX_BACKEND,
  WP_CODEBOX_AGENT_FANOUT_REQUEST_SCHEMA,
  WP_CODEBOX_PROVIDER_ID,
  WP_CODEBOX_PROVIDER_LABEL,
  WP_CODEBOX_PROVIDER_RUNTIME_ABILITY_NAMES,
  WP_CODEBOX_PROVIDER_RUNTIME_INVOCATION_CONTRACT_SCHEMA,
  WP_CODEBOX_PROVIDER_RUNTIME_RESULT_SCHEMAS,
  WP_CODEBOX_PROVIDER_RUNTIME_TASK_NAMES,
  WP_CODEBOX_ROLE_ALIASES,
  WP_CODEBOX_TASK_REQUEST_SCHEMA,
  WP_CODEBOX_UPSTREAM_PRIMITIVE_REQUIREMENTS,
  WP_CODEBOX_WORKSPACE_MOUNT_KIND,
  wpCodeboxAgentFanoutAdapterContract,
  wpCodeboxProviderRuntimeInvocationContract,
  wpCodeboxProviderRuntimeOperationEntry,
} = require('./wp-codebox-adapter-contract');
const {
  WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA,
} = require('./codebox-run-agent-task-contract');
const {
  wpCodeboxBin,
} = require('./wp-codebox-adapter-descriptor');
const {
  assertProviderCredentialBoundaryNamesOnly,
  providerCredentialBoundary,
  providerCredentialRequestFields,
} = require('./provider-credential-boundary');

const RUNTIME_MANIFEST_PATH = path.resolve(__dirname, '..', 'wp-codebox.json');
const RUNTIME_OVERLAY_CANONICAL_SHAPE = 'runtime_overlays entries must be objects. WP Codebox owns the runtime overlay schema and reports field-level validation.';
const RUNTIME_EXECUTION_DESCRIPTOR_SCHEMA = 'homeboy/runtime-execution/v1';
const AGENT_TASK_EVENT_SCHEMA = 'homeboy/agent-task-event/v1';
const PROVIDER_CAPABILITIES = runtimeProviderCapabilities();
const WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY = 'wp-codebox/run-runtime-package';
const NEUTRAL_RUNTIME_PACKAGE_ABILITIES = new Set(['homeboy/run-runtime-package']);
const LEGACY_RUNTIME_PACKAGE_ABILITIES = new Set(['agents/run-runtime-package', 'runtime-package/run']);
const RUNTIME_PACKAGE_ABILITY_ALIASES = new Set([
  ...NEUTRAL_RUNTIME_PACKAGE_ABILITIES,
  ...LEGACY_RUNTIME_PACKAGE_ABILITIES,
]);
const LEGACY_RUNTIME_PACKAGE_ABILITY_QUARANTINE = 'legacy-runtime-package-ability-alias';

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

const WP_CODEBOX_RUNTIME_GAP_TRACKERS = [
  {
    gap: 'runtime-profile-normalizer',
    needed_primitive: 'WP Codebox should export a stable runtime-profile builder/normalizer that accepts Homeboy-provided component contracts, provider plugin paths, runtime env, overlays, and parent-tool bridge declarations without Homeboy reshaping dependency fields locally.',
  },
  {
    gap: 'typed-artifact-dto-normalizer',
    needed_primitive: 'WP Codebox should export a stable typed-artifact DTO normalizer compatible with Homeboy agent-task typed artifact declarations, including file_refs/fileRefs and artifact_schema/artifactSchema aliases while allowing caller-owned metadata redaction policy.',
  },
];
const WP_CODEBOX_BUILTIN_ARTIFACT_DECLARATION_NAMES = new Set(['patch', 'agent_result', 'transcript']);

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
    runtime_execution_contracts: runtimeExecutionContracts(),
    workspace_materialization: {
      cwd: 'git_checkout',
    },
    runner_readiness: runtimeRunnerReadiness(options),
    runner_sources: runtimeRunnerSources(options),
    workspace_tools: runtimeWorkspaceTools(options),
    component_path_defaults: runtimeComponentPathDefaults(options),
    provider_metadata: runtimeExecutorManifest().provider_metadata,
    provider_defaults: providerDefaultsContract(runtimeProviderDefaults()),
    provider_preflight: runtimeProviderPreflight(),
    provider_credential_boundary: providerCredentialBoundary(),
    provider_runtime_invocation: providerRuntimeInvocationContract(),
    agent_fanout_adapter: wpCodeboxAgentFanoutAdapterContract(),
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

function runtimeExecutionContracts() {
  return firstObject(runtimeExecutorManifest().runtime_execution_contracts, runtimeExecutorManifest().execution_contracts) || {};
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

function runtimeProviderDefaults() {
  return firstObject(runtimeExecutorManifest().provider_defaults) || {};
}

function runtimeProviderPreflight() {
  return firstObject(runtimeExecutorManifest().provider_preflight) || {};
}

function runtimeRunnerReadiness(options = {}) {
  return normalizeArray(options.runnerReadiness || options.runner_readiness || runtimeExecutorManifest().runner_readiness);
}

function runtimeRunnerSources(options = {}) {
  return normalizeArray(options.runnerSources || options.runner_sources || runtimeExecutorManifest().runner_sources);
}

function runtimeSecretEnvRequirements() {
  return normalizeArray(runtimeExecutorManifest().secret_env_requirements);
}

function codeboxTaskRequestFromAgentTaskRequest(request, options = {}) {
  assertAgentTaskRequest(request);
  const config = request.executor.config || {};
  assertProviderCredentialBoundaryNamesOnly(request.executor || {});
  assertProviderCredentialBoundaryNamesOnly(config);
  assertProviderCredentialBoundaryNamesOnly(request.inputs || {});
  const runtimeOptions = runtimeOptionsFromExecutorConfig(config, options);
  const inputs = request.inputs || {};
  const clientContext = agentTaskClientContext(request, config, inputs, runtimeOptions);
  const clientInputs = firstObject(clientContext.inputs) || {};
  const defaults = defaultCodeboxRuntimeConfig(request, config, inputs, runtimeOptions);
  const workspaceMaterialization = defaultWorkspaceMaterialization(defaults.workspaceRoot, request, config, inputs, runtimeOptions);
  const target = defaultWorkspaceTargetPayload(inputs.target || request.workspace || {}, workspaceMaterialization);
  const agentBundle = agentBundleConfigFromAgentTaskRequest(request, config, inputs);
  const recipe = recipeConfigFromAgentTaskRequest(request, config, inputs, runtimeOptions);
  const runtimeRequirementsForMounts = firstObject(config.runtime_requirements, config.runtimeRequirements) || {};
  const mounts = agentBundleMounts(agentBundle, config.runtime_mounts || config.mounts || runtimeRequirementsForMounts.runtime_mounts || runtimeRequirementsForMounts.mounts || defaults.mounts || runtimeOptions.mounts || []);
  const agentBundles = firstDefined(inputs.agent_bundles, inputs.agentBundles, config.agent_bundles, config.agentBundles, runtimeOptions.agentBundles, []);
  const provider = config.provider || runtimeOptions.provider || defaults.provider || '';
  const model = request.executor.model || config.model || runtimeOptions.model || defaults.model || '';
  const runtimeTask = runtimeTaskWithExecutionDefaults(
    inputs.runtime_task || inputs.runtimeTask || clientInputs.runtime_task || clientInputs.runtimeTask || config.runtime_task || config.runtimeTask || abilityRuntimeTaskFromAgentTaskRequest(request, config, inputs) || runtimeOptions.runtimeTask,
    { provider, model, agentBundles, runtimePackage: runtimePackageDefaultFromProfile(config, runtimeOptions) }
  );
  let componentContracts = componentContractsFromAgentTaskRequest(request, config, runtimeOptions);
  let components = runtimeComponentPaths(config, { ...defaults, ...runtimeOptions, componentContracts });
  const structuredArtifacts = firstDefined(inputs.structured_artifacts, inputs.structuredArtifacts, config.structured_artifacts, config.structuredArtifacts, runtimeOptions.structuredArtifacts, []);
  const artifactDeclarations = codeboxTaskArtifactDeclarations(artifactDeclarationsFromAgentTaskRequest(request, config, inputs, runtimeOptions));
  const homeboyToolPolicy = homeboyAgentToolPolicy();
  const allowedTools = allowedToolsFromAgentTaskRequest(request, config, inputs, runtimeOptions, defaults);
  const sandboxToolPolicy = sandboxToolPolicyFromAgentTaskRequest(config, inputs, runtimeOptions, defaults, allowedTools, homeboyToolPolicy);
  const sandboxAllowedTools = allowedToolsForHomeboyToolPolicy(allowedTools, homeboyToolPolicy);
  const homeboySecretEnvPlan = homeboyAgentTaskSecretEnvPlan();
  const plannedSecretEnv = secretEnvNamesFromPlan(homeboySecretEnvPlan);
  const agent = firstValue(config.agent, runtimeOptions.agent, '');
  const abilityRequirements = runtimeAbilityRequirements(runtimeTask, request, config, inputs, runtimeOptions);
  const providerRuntimeInvocation = providerRuntimeInvocationFromConfig(config, inputs, runtimeOptions);
  const codeboxFanoutRequest = codeboxFanoutRequestFromAgentTaskRequest(request, config, inputs, runtimeOptions);
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
  const runtimeTaskAbilityNormalization = runtimeTaskAbilityNormalizationEvidence(runtimeTask);
  const context = {
    ...clientContext,
    ...(clientInputs.context || {}),
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
    goal: request.goal || request.instructions,
    target,
    workspace_materialization: workspaceMaterialization,
    allowed_tools: sandboxAllowedTools || [],
    expected_artifacts: expectedArtifactsForCodeboxTask(request, artifactDeclarations),
    artifact_declarations: artifactDeclarations,
    policy: request.policy || {},
    context,
    recipe,
    sandbox_tool_policy: sandboxToolPolicy,
    runtime_task: runtimeTask,
    ...(runtimeTaskAbilityNormalization ? { runtime_task_ability_normalization: runtimeTaskAbilityNormalization } : {}),
    ability_requirements: abilityRequirements,
    callback_data: firstDefined(inputs.callback_data, inputs.callbackData, config.callback_data, config.callbackData, runtimeOptions.callbackData),
    provider_runtime_invocation: providerRuntimeInvocation,
    ...(codeboxFanoutRequest ? { fanout_request: codeboxFanoutRequest } : {}),
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
    ...providerCredentialRequestFields({ secret_env: explicitSecretEnv.length > 0 ? explicitSecretEnv : defaults.secretEnv || [] }),
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
    wp_codebox_bin: wpCodeboxBin({
      runtime_bin: config.runtime_bin,
      wp_codebox_bin: config.wp_codebox_bin,
      wpCodeboxBin: config.wpCodeboxBin || runtimeOptions.wpCodeboxBin || defaults.wpCodeboxBin,
      env: process.env,
      executable: '',
      preferPackagedRuntime: Boolean(process.env.HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT || process.env.WP_CODEBOX_RUNTIME_COMPONENT),
    }),
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

function codeboxFanoutRequestFromAgentTaskRequest(request, config = {}, inputs = {}, runtimeOptions = {}) {
  const source = firstObject(
    inputs.codebox_fanout_request,
    inputs.codeboxFanoutRequest,
    inputs.fanout_request,
    inputs.fanoutRequest,
    config.codebox_fanout_request,
    config.codeboxFanoutRequest,
    config.fanout_request,
    config.fanoutRequest,
    runtimeOptions.codeboxFanoutRequest,
    runtimeOptions.fanoutRequest
  );
  if (!source) {
    return null;
  }
  const workers = normalizeArray(source.workers).map((worker, index) => {
    const metadata = firstObject(worker.metadata) || {};
    const runtimeTask = fanoutWorkerRuntimeTask(worker, request, config, runtimeOptions);
    const runtimeTaskAbilityNormalization = runtimeTaskAbilityNormalizationEvidence(runtimeTask);
    return withoutUndefinedValues({
      ...worker,
      id: firstValue(worker.id, worker.worker_id, `${request.task_id}-worker-${index + 1}`),
      goal: firstValue(worker.goal, worker.task, request.instructions),
      agent: firstValue(worker.agent, source.agent, config.agent, runtimeOptions.agent),
      dependsOn: normalizeArray(firstValue(worker.dependsOn, worker.depends_on)),
      artifactNamespace: firstValue(worker.artifactNamespace, worker.artifact_namespace, `${request.task_id}/${index + 1}`),
      ...(runtimeTask ? { runtime_task: runtimeTask } : {}),
      ...(runtimeTaskAbilityNormalization ? { runtime_task_ability_normalization: runtimeTaskAbilityNormalization } : {}),
      ability_requirements: runtimeTask?.ability ? uniqueStrings([runtimeTask.ability, ...normalizeArray(worker.ability_requirements), ...normalizeArray(worker.abilityRequirements)]) : firstDefined(worker.ability_requirements, worker.abilityRequirements),
      metadata: {
        ...metadata,
        homeboy_task_id: request.task_id,
        parent_plan_id: request.parent_plan_id,
        group_key: request.group_key,
      },
    });
  });

  return withoutUndefinedValues({
    ...source,
    schema: WP_CODEBOX_AGENT_FANOUT_REQUEST_SCHEMA,
    workers,
    agent: firstValue(source.agent, config.agent, runtimeOptions.agent),
    orchestrator: {
      ...(firstObject(source.orchestrator) || {}),
      agent_task_id: request.task_id,
      parent_plan_id: request.parent_plan_id,
      group_key: request.group_key,
      provider: firstValue(config.provider, runtimeOptions.provider),
      model: firstValue(request.executor?.model, config.model, runtimeOptions.model),
      secret_env_names: normalizeArray(request.executor?.secret_env),
    },
    metadata: {
      ...(firstObject(source.metadata) || {}),
      homeboy_agent_task: {
        task_id: request.task_id,
        parent_plan_id: request.parent_plan_id,
        group_key: request.group_key,
      },
    },
  });
}

function fanoutWorkerRuntimeTask(worker = {}, request = {}, config = {}, runtimeOptions = {}) {
  const runtimeTask = firstObject(worker.runtime_task, worker.runtimeTask);
  if (runtimeTask) {
    return runtimeTaskWithExecutionDefaults(runtimeTask, fanoutWorkerRuntimeTaskDefaults(worker, request, config, runtimeOptions));
  }

  const abilityRequest = firstObject(worker.ability_request, worker.abilityRequest);
  const ability = firstValue(
    abilityRequest?.id,
    abilityRequest?.name,
    abilityRequest?.ability,
    typeof worker.ability === 'string' ? worker.ability : '',
    worker.ability_name,
    worker.abilityName
  );
  if (!ability || typeof ability !== 'string') {
    return null;
  }

  return runtimeTaskWithExecutionDefaults({
    ability,
    input: firstObject(abilityRequest?.input, abilityRequest?.args, worker.ability_input, worker.abilityInput, worker.input) || {},
  }, fanoutWorkerRuntimeTaskDefaults(worker, request, config, runtimeOptions));
}

function fanoutWorkerRuntimeTaskDefaults(worker = {}, request = {}, config = {}, runtimeOptions = {}) {
  return {
    provider: firstValue(worker.provider, config.provider, runtimeOptions.provider),
    model: firstValue(worker.model, request.executor?.model, config.model, runtimeOptions.model),
    agentBundles: firstDefined(worker.agent_bundles, worker.agentBundles, runtimeOptions.agentBundles, []),
    runtimePackage: firstValue(worker.runtime_package, worker.runtimePackage, runtimePackageDefaultFromProfile(config, runtimeOptions)),
  };
}

function expectedArtifactsForCodeboxTask(request, artifactDeclarations = []) {
  const declarationNames = artifactDeclarations
    .filter((declaration) => declaration && declaration.required === true)
    .map((declaration) => typedArtifactNameFromDeclaration(declaration))
    .filter(Boolean);
  if (declarationNames.length > 0) {
    return Array.from(new Set(declarationNames));
  }
  return normalizeArray(request.expected_artifacts);
}

function runtimeAbilityRequirements(runtimeTask, request = {}, config = {}, inputs = {}, options = {}) {
  return uniqueStrings([
    runtimeTask?.ability,
    runtimeTask?.runtime_task_ability,
    ...normalizeArray(request.ability_requirements),
    ...normalizeArray(request.abilityRequirements),
    ...normalizeArray(inputs.ability_requirements),
    ...normalizeArray(inputs.abilityRequirements),
    ...normalizeArray(config.ability_requirements),
    ...normalizeArray(config.abilityRequirements),
    ...normalizeArray(options.abilityRequirements),
    ...normalizeArray(options.ability_requirements),
  ]);
}

function codeboxTaskArtifactDeclarations(artifactDeclarations = []) {
  const declarations = normalizeArray(artifactDeclarations);
  const taskSpecificDeclarations = declarations.filter((declaration) => {
    const name = typedArtifactNameFromDeclaration(declaration);
    return name && !WP_CODEBOX_BUILTIN_ARTIFACT_DECLARATION_NAMES.has(name);
  });
  return taskSpecificDeclarations.length > 0 ? taskSpecificDeclarations : declarations;
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
    mounts: firstDefined(runtimeRequirements.runtime_mounts, runtimeRequirements.mounts, runtimeProfile.runtime_mounts, runtimeProfile.mounts, options.mounts),
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

function runtimePackageDefaultFromProfile(config = {}, runtimeOptions = {}) {
  const runtimeProfile = firstObject(runtimeOptions.runtimeProfile) || {};
  return firstValue(runtimeProfile.runtime_package, runtimeProfile.runtimePackage, runtimeProfile.package, config.runtime_profile, config.runtimeProfile, runtimeProfile.id);
}

function artifactDeclarationsFromAgentTaskRequest(request, config = {}, inputs = {}, options = {}) {
  const declarations = firstNonEmptyArray(
    request.artifact_declarations,
    options.artifactDeclarations,
    []
  )
    .map((declaration) => wpCodeboxArtifactDeclarationFromHomeboy(declaration))
    .filter(Boolean);
  return uniqueArtifactDeclarations([
    ...declarations,
    ...legacyArtifactDeclarationsFromAgentTaskRequest(request, config, inputs, options),
  ]);
}

function uniqueArtifactDeclarations(declarations) {
  const seen = new Set();
  return declarations.filter((declaration) => {
    const name = typedArtifactNameFromDeclaration(declaration);
    const schema = declaration?.artifact_schema || declaration?.artifactSchema || declaration?.schema || '';
    const key = `${name}:${schema}`;
    if (!name || seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
}

function legacyArtifactDeclarationsFromAgentTaskRequest(request, config = {}, inputs = {}, options = {}) {
  const clientContext = agentTaskClientContext(request, config, inputs, options);
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
    clientContext.artifacts,
    clientContext.artifact_outputs,
    clientContext.artifactOutputs,
    clientContext.output_artifacts,
    clientContext.outputArtifacts,
    clientContext.outputs?.artifacts,
    clientContext.outputs?.artifact_outputs,
    clientContext.outputs?.artifactOutputs,
    clientContext.outputs?.typed_artifacts,
    clientContext.outputs?.typedArtifacts,
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
  let normalizedDeclaration = declaration;
  if (declaration && typeof declaration === 'object' && !Array.isArray(declaration)) {
    normalizedDeclaration = { ...declaration };
    if (!normalizedDeclaration.name && !normalizedDeclaration.id && normalizedDeclaration.artifact_id) {
      normalizedDeclaration.name = normalizedDeclaration.artifact_id;
    }
    if (typeof normalizedDeclaration.kind === 'string' && normalizedDeclaration.kind.includes('/') && !normalizedDeclaration.artifact_schema && !normalizedDeclaration.artifactSchema) {
      normalizedDeclaration.artifact_schema = normalizedDeclaration.kind;
      if (!normalizedDeclaration.type && !normalizedDeclaration.artifact_type && !normalizedDeclaration.artifactType) {
        delete normalizedDeclaration.kind;
      }
    }
  }
  return normalizeCodeboxArtifactDeclaration(defaultName, normalizedDeclaration, {
    ignoredSchemas: [AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA],
  });
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
  const runtimeRequirements = mergeRuntimeRequirements(defaults.runtimeRequirements, firstObject(config.runtime_requirements, config.runtimeRequirements) || {});
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
    normalizeRuntimeProfile: options.normalizeRuntimeProfile,
    normalizeRuntimeProfilePayload: options.normalizeRuntimeProfilePayload,
  });
}

function mergeRuntimeRequirements(...requirements) {
  const normalized = requirements.filter((requirement) => requirement && typeof requirement === 'object' && !Array.isArray(requirement));
  if (normalized.length === 0) {
    return {};
  }
  return {
    ...Object.assign({}, ...normalized),
    ability_requirements: uniqueStrings(normalized.flatMap((requirement) => normalizeArray(requirement.ability_requirements || requirement.abilityRequirements))),
    component_contracts: uniqueRuntimeRequirementObjects(normalized.flatMap((requirement) => normalizeArray(requirement.component_contracts))),
    extra_plugins: uniqueRuntimeRequirementObjects(normalized.flatMap((requirement) => normalizeArray(requirement.extra_plugins))),
    components: uniqueRuntimeRequirementObjects(normalized.flatMap((requirement) => normalizeArray(requirement.components))),
    mu_plugins: uniqueRuntimeRequirementObjects(normalized.flatMap((requirement) => normalizeArray(requirement.mu_plugins))),
    plugins: uniqueRuntimeRequirementObjects(normalized.flatMap((requirement) => normalizeArray(requirement.plugins))),
    runtime_overlays: uniqueRuntimeRequirementObjects(normalized.flatMap((requirement) => normalizeArray(requirement.runtime_overlays))),
  };
}

function uniqueRuntimeRequirementObjects(entries) {
  const seen = new Set();
  return normalizeArray(entries).filter((entry) => {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      return false;
    }
    const key = [entry.slug, entry.id, entry.path || entry.source || entry.target, entry.kind, entry.type].filter(Boolean).join(':');
    if (!key) {
      return true;
    }
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
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
  const normalizedAbility = normalizeRuntimeTaskAbilityForCodebox(ability);
  const abilityNormalization = runtimeTaskAbilityNormalization({ requestedAbility: ability, normalizedAbility });
  if (RUNTIME_PACKAGE_ABILITY_ALIASES.has(ability) || normalizedAbility === WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY) {
    return {
      ability: normalizedAbility,
      ...(abilityNormalization ? { ability_normalization: abilityNormalization } : {}),
      input: runtimePackageTaskInputForCodebox(agentBundleRuntimeTaskInputWithArtifactOutputs(input, request, config, inputs)),
    };
  }
  return { ability: normalizedAbility, ...(abilityNormalization ? { ability_normalization: abilityNormalization } : {}), input };
}

function normalizeRuntimeTaskAbilityForCodebox(ability) {
  return RUNTIME_PACKAGE_ABILITY_ALIASES.has(ability) ? WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY : ability;
}

function runtimePackageTaskInputForCodebox(input) {
  if (!input || typeof input !== 'object' || Array.isArray(input)) {
    return input;
  }
  const normalized = { ...input };
  const options = normalized.options && typeof normalized.options === 'object' && !Array.isArray(normalized.options) ? normalized.options : {};
  for (const key of ['provider', 'model']) {
    if (!firstValue(normalized[key]) && firstValue(options[key])) {
      normalized[key] = options[key];
    }
  }
  const packageDescriptor = normalized.runtime_package === undefined ? normalized.package : normalized.runtime_package;
  const runtimePackage = runtimePackageIdentifier(packageDescriptor);
  if (runtimePackage) {
    normalized.runtime_package = runtimePackage;
    if (!firstValue(normalized.agent, normalized.agent_slug)) {
      normalized.agent = runtimePackage;
    }
  }
  if (packageDescriptor && typeof packageDescriptor === 'object' && !Array.isArray(packageDescriptor)) {
    normalized.metadata = {
      ...(normalized.metadata && typeof normalized.metadata === 'object' && !Array.isArray(normalized.metadata) ? normalized.metadata : {}),
      runtime_package_descriptor: packageDescriptor,
    };
  }
  delete normalized.package;
  return normalized;
}

function runtimePackageIdentifier(value) {
  if (typeof value === 'string') {
    return value;
  }
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return '';
  }
  return firstValue(value.slug, value.id, value.name, value.source, '');
}

function agentBundleRuntimeTaskInputWithArtifactOutputs(input, request, config, inputs) {
  const declarations = codeboxTaskArtifactDeclarations(artifactDeclarationsFromAgentTaskRequest(request, config, inputs))
    .filter((declaration) => declaration && typeof declaration === 'object' && declaration.required === true && typedArtifactNameFromDeclaration(declaration));
  if (declarations.length === 0) {
    return input;
  }

  const requiredArtifacts = Array.from(new Set([
    ...normalizeArray(input.required_artifacts),
    ...declarations.map((declaration) => typedArtifactNameFromDeclaration(declaration)).filter(Boolean),
  ]));
  const engineDataOutputs = {
    ...(input.engine_data_outputs && typeof input.engine_data_outputs === 'object' && !Array.isArray(input.engine_data_outputs) ? input.engine_data_outputs : {}),
  };
  for (const declaration of declarations) {
    const name = typedArtifactNameFromDeclaration(declaration);
    if (name && !engineDataOutputs[name]) {
      engineDataOutputs[name] = `outputs.typed_artifacts.${name}.payload`;
    }
  }

  return {
    ...input,
    required_artifacts: requiredArtifacts,
    engine_data_outputs: engineDataOutputs,
  };
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
    client_context: agentTaskClientContext(request, config, inputs),
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
  return firstObject(agentTaskClientContext(request, config, inputs).inputs) || {};
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
  const requestedAbility = runtimeTask.ability;
  const normalizedAbility = typeof requestedAbility === 'string' ? normalizeRuntimeTaskAbilityForCodebox(requestedAbility) : requestedAbility;
  const normalizedRuntimeTask = {
    ...runtimeTask,
    ...(typeof requestedAbility === 'string' ? { ability: normalizedAbility } : {}),
  };
  const abilityNormalization = runtimeTask.ability_normalization || runtimeTaskAbilityNormalization({ requestedAbility, normalizedAbility });
  if (abilityNormalization) {
    normalizedRuntimeTask.ability_normalization = abilityNormalization;
  }
  if (normalizedRuntimeTask.ability === WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY) {
    normalizedRuntimeTask.input = runtimePackageTaskInputForCodebox(normalizedRuntimeTask.input);
  }
  const applyExecutionDefaults = normalizedRuntimeTask.ability === WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY || (Array.isArray(defaults.agentBundles) && defaults.agentBundles.length > 0);
  if (!applyExecutionDefaults) {
    return normalizedRuntimeTask;
  }

  const input = normalizedRuntimeTask.input && typeof normalizedRuntimeTask.input === 'object' && !Array.isArray(normalizedRuntimeTask.input)
    ? normalizedRuntimeTask.input
    : {};
  const defaultInput = Object.fromEntries(Object.entries({
    runtime_package: defaults.runtimePackage,
    provider: defaults.provider,
    model: defaults.model,
  }).filter(([, value]) => value !== '' && value !== undefined));

  if (Object.keys(defaultInput).length === 0) {
    return normalizedRuntimeTask;
  }

  return {
    ...normalizedRuntimeTask,
    input: normalizedRuntimeTask.ability === WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY ? runtimePackageTaskInputForCodebox({
      ...defaultInput,
      ...input,
    }) : {
      ...defaultInput,
      ...input,
    },
  };
}

function runtimeTaskAbilityNormalization({ requestedAbility, normalizedAbility }) {
  if (typeof normalizedAbility !== 'string') {
    return null;
  }
  if (requestedAbility === normalizedAbility && normalizedAbility !== WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY) {
    return null;
  }
  const runtimePackage = normalizedAbility === WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY;
  const legacyDeprecation = legacyRuntimePackageAbilityDeprecation(requestedAbility, normalizedAbility);
  return {
    schema: 'wp-codebox/runtime-task-ability-normalization/v1',
    requested_ability: requestedAbility || normalizedAbility,
    normalized_codebox_ability: normalizedAbility,
    bridge_ability: runtimePackage ? WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY : normalizedAbility,
    runtime_ability: normalizedAbility,
    owning_components: ['wp-codebox'],
    ...(legacyDeprecation ? { deprecated_compatibility_alias: legacyDeprecation } : {}),
  };
}

function legacyRuntimePackageAbilityDeprecation(requestedAbility, normalizedAbility) {
  if (!LEGACY_RUNTIME_PACKAGE_ABILITIES.has(requestedAbility) || normalizedAbility !== WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY) {
    return null;
  }
  return {
    schema: 'wp-codebox/deprecated-compatibility-alias/v1',
    alias: requestedAbility,
    replacement: WP_CODEBOX_RUN_RUNTIME_PACKAGE_ABILITY,
    quarantine: LEGACY_RUNTIME_PACKAGE_ABILITY_QUARANTINE,
    status: 'deprecated',
  };
}

function runtimeTaskAbilityNormalizationEvidence(runtimeTask = {}) {
  const normalization = runtimeTask?.ability_normalization;
  if (!normalization || typeof normalization !== 'object' || Array.isArray(normalization)) {
    return null;
  }
  return normalization;
}

function runtimeComponentPaths(config, options = {}) {
  const runtimeComponents = {
    ...(config.runtime_components && typeof config.runtime_components === 'object' ? config.runtime_components : {}),
    ...(process.env.HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT ? { runtime: process.env.HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT } : {}),
  };
  const explicit = config.runtime_component_paths && typeof config.runtime_component_paths === 'object'
    ? config.runtime_component_paths
    : {};
  const contractPaths = runtimeComponentPathsFromContracts(config.component_contracts || options.componentContracts || [], options);
  const aliases = runtimeComponentPathAliases(options);
  const resolved = {
    ...contractPaths,
    agents_api: contractPaths.agents_api || firstValue(process.env.WP_CODEBOX_AGENTS_API_PATH, process.env.HOMEBOY_WP_CODEBOX_AGENTS_API_PATH),
    agent_runtime: contractPaths.agent_runtime || explicit.agent_runtime || config.agent_runtime || options.agentRuntime,
    agent_runtime_tools: config.agent_runtime_tools || options.agentRuntimeTools,
    data_machine_code: contractPaths.data_machine_code || firstValue(process.env.WP_CODEBOX_DATA_MACHINE_CODE_PATH, process.env.HOMEBOY_WP_CODEBOX_DATA_MACHINE_CODE_PATH),
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

  resolved.agent_runtime = resolved.agent_runtime || firstValue(process.env.WP_CODEBOX_DATA_MACHINE_PATH, process.env.HOMEBOY_WP_CODEBOX_DATA_MACHINE_PATH);

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
  const phpAiClientPath = defaultPhpAiClientPath(settings, options);

  return {
    agentRuntime: agentRuntimePath,
    agentRuntimeTools: agentRuntimeToolsPath,
    legacyRuntime: agentRuntimePath,
    legacyRuntimeTools: agentRuntimeToolsPath,
    providerPluginPaths: defaultProviderPluginPaths(provider, config, options, settings, providerConfig, providerPluginPath),
    provider,
    model,
    secretEnv: defaultSecretEnv(config, options, settings, providerConfig),
    wpCodeboxBin: wpCodeboxBin({ settings, executable: '' }),
    runtimeOverlayProfiles: defaultRuntimeOverlayProfiles(settings),
    runtimeOverlays: defaultRuntimeOverlays(settings, phpAiClientPath),
    runtimeRequirements: defaultRuntimeRequirements(),
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

function defaultRuntimeRequirements() {
  return {};
}

function defaultChatHandlerPluginContracts(settings = {}, options = {}, providerConfig = {}) {
  return uniqueRuntimeRequirementObjects([
    ...chatHandlerPluginEntries(options.chatHandlerPluginPaths || options.chat_handler_plugin_paths),
    ...chatHandlerPluginEntries(settings.wp_codebox_chat_handler_plugin_paths || settings.chat_handler_plugin_paths),
    ...chatHandlerPluginEntries(providerConfig.chat_handler_plugin_paths),
    ...chatHandlerPluginEntries(envPathList(process.env.HOMEBOY_WP_CODEBOX_CHAT_HANDLER_PLUGIN_PATHS)),
    ...chatHandlerPluginEntries(envPathList(process.env.WP_CODEBOX_CHAT_HANDLER_PLUGIN_PATHS)),
  ]);
}

function chatHandlerPluginEntries(value) {
  return normalizeArray(value).map((entry) => chatHandlerPluginContract(entry)).filter(Boolean);
}

function chatHandlerPluginContract(entry) {
  const source = typeof entry === 'string'
    ? entry
    : entry?.path || entry?.source || entry?.target || '';
  const slug = typeof entry === 'object' && entry
    ? entry.slug || entry.id || slugFromRuntimePath(source)
    : slugFromRuntimePath(source);
  if (!source || !slug) {
    return null;
  }
  const explicit = typeof entry === 'object' && entry ? entry : {};
  return {
    ...explicit,
    slug,
    path: explicit.path || explicit.source || source,
    pluginFile: explicit.pluginFile || explicit.plugin_file || `${slug}/${slug}.php`,
    loadAs: explicit.loadAs || explicit.load_as || 'plugin',
    activate: explicit.activate === undefined ? true : explicit.activate,
    metadata: {
      ...(explicit.metadata || {}),
      source: explicit.metadata?.source || 'homeboy-extensions-codebox-chat-handler-default',
      registers: uniquePaths([
        ...normalizeArray(explicit.metadata?.registers),
        'wp_agent_chat_handler',
      ]),
    },
  };
}

function slugFromRuntimePath(source = '') {
  return source ? path.basename(String(source).replace(/\/+$/, '')) : '';
}

function envPathList(value) {
  if (!value) {
    return [];
  }
  try {
    const parsed = JSON.parse(value);
    if (Array.isArray(parsed)) {
      return parsed;
    }
  } catch {
    // Fall through to PATH-style lists for simple environment configuration.
  }
  return String(value).split(path.delimiter).map((entry) => entry.trim()).filter(Boolean);
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

function agentTaskClientContext(request = {}, config = {}, inputs = {}, options = {}) {
  return firstObject(
    inputs.client_context,
    inputs.clientContext,
    request.client_context,
    request.clientContext,
    config.client_context,
    config.clientContext,
    options.clientContext,
    parseJsonObject(inputs.client_context),
    parseJsonObject(inputs.clientContext),
    parseJsonObject(request.client_context),
    parseJsonObject(request.clientContext),
    parseJsonObject(request.dispatch?.client_context),
    parseJsonObject(request.dispatch?.clientContext),
    parseJsonObject(config.client_context),
    parseJsonObject(config.clientContext),
  ) || {};
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
      metadata: { kind: WP_CODEBOX_WORKSPACE_MOUNT_KIND, workspace_slug: workspaceSlug(workspaceRoot), workspaceRef: path.basename(workspaceRoot) },
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

function withoutUndefinedValues(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function recipeConfigFromAgentTaskRequest(request, config, inputs, runtimeOptions = {}) {
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
    inputs: recipeInputsFromAgentTaskRequest(config, inputs, explicit, runtimeOptions),
    secret_env: explicit.secret_env || explicit.secretEnv || inputs.recipe_secret_env || inputs.recipeSecretEnv || config.recipe_secret_env || config.recipeSecretEnv,
    metadata: explicit.metadata,
  }).filter(([, value]) => value !== undefined && value !== '' && !(Array.isArray(value) && value.length === 0)));

  return recipe.pack || recipe.name || recipe.path || recipe.repository || recipe.target_ref ? recipe : {};
}

function recipeInputsFromAgentTaskRequest(config = {}, inputs = {}, explicit = {}, runtimeOptions = {}) {
  const recipeInputs = firstObject(explicit.inputs, inputs.recipe_inputs, inputs.recipeInputs, config.recipe_inputs, config.recipeInputs) || {};
  const matrixInputs = roleCapabilityMatrixRecipeInputs(config, inputs, runtimeOptions);
  if (!matrixInputs.fixtureUsers && !matrixInputs.userSessions) {
    return Object.keys(recipeInputs).length > 0 ? recipeInputs : undefined;
  }
  return {
    ...recipeInputs,
    fixtureUsers: mergeRecipeEntriesByName(recipeInputs.fixtureUsers, matrixInputs.fixtureUsers),
    userSessions: mergeRecipeEntriesByName(recipeInputs.userSessions, matrixInputs.userSessions),
  };
}

function roleCapabilityMatrixRecipeInputs(config = {}, inputs = {}, runtimeOptions = {}) {
  const runtimeRequirements = firstObject(config.runtime_requirements, config.runtimeRequirements) || {};
  const runtimeProfile = firstObject(runtimeOptions.runtimeProfile) || {};
  const matrix = firstDefined(
    inputs.role_matrix,
    inputs.roleMatrix,
    inputs.capability_matrix,
    inputs.capabilityMatrix,
    config.role_matrix,
    config.roleMatrix,
    config.capability_matrix,
    config.capabilityMatrix,
    runtimeRequirements.role_matrix,
    runtimeRequirements.roleMatrix,
    runtimeRequirements.capability_matrix,
    runtimeRequirements.capabilityMatrix,
    runtimeProfile.role_matrix,
    runtimeProfile.roleMatrix,
    runtimeProfile.capability_matrix,
    runtimeProfile.capabilityMatrix
  );
  const entries = roleCapabilityMatrixEntries(matrix);
  if (entries.length === 0) {
    return {};
  }
  return {
    fixtureUsers: entries.map((entry) => roleMatrixFixtureUser(entry)),
    userSessions: entries.map((entry) => roleMatrixUserSession(entry)),
  };
}

function roleCapabilityMatrixEntries(matrix) {
  if (Array.isArray(matrix)) {
    return matrix.map((entry) => normalizeRoleCapabilityMatrixEntry(entry)).filter(Boolean);
  }
  if (!matrix || typeof matrix !== 'object') {
    return [];
  }
  return Object.entries(matrix).map(([role, value]) => normalizeRoleCapabilityMatrixEntry({ role, value })).filter(Boolean);
}

function normalizeRoleCapabilityMatrixEntry(entry) {
  if (typeof entry === 'string') {
    return { role: entry, name: sanitizeRecipeName(entry), capabilities: [] };
  }
  if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
    return null;
  }
  const value = entry.value && typeof entry.value === 'object' && !Array.isArray(entry.value) ? entry.value : {};
  const role = firstValue(entry.role, value.role, entry.name, value.name);
  const name = sanitizeRecipeName(firstValue(entry.name, value.name, role));
  if (!role || !name) {
    return null;
  }
  return {
    name,
    role: String(role),
    username: firstValue(entry.username, value.username),
    email: firstValue(entry.email, value.email),
    displayName: firstValue(entry.displayName, entry.display_name, value.displayName, value.display_name),
    password: firstValue(entry.password, value.password),
    capabilities: normalizeArray(firstDefined(entry.capabilities, entry.capability, value.capabilities, value.capability, Array.isArray(entry.value) ? entry.value : [])).map(String),
    sessionName: sanitizeRecipeName(firstValue(entry.session, entry.sessionName, entry.session_name, value.session, value.sessionName, value.session_name, `${name}-session`)),
  };
}

function roleMatrixFixtureUser(entry) {
  return withoutEmptyObjectValues({
    name: entry.name,
    username: entry.username || `fixture-${entry.name}`,
    email: entry.email,
    role: entry.role,
    displayName: entry.displayName,
    password: entry.password,
    metadata: withoutEmptyObjectValues({ capabilities: entry.capabilities }),
  });
}

function roleMatrixUserSession(entry) {
  return withoutEmptyObjectValues({
    name: entry.sessionName,
    user: entry.name,
    metadata: withoutEmptyObjectValues({ role: entry.role, capabilities: entry.capabilities }),
  });
}

function mergeRecipeEntriesByName(explicitEntries, generatedEntries) {
  const entries = [];
  const seen = new Set();
  for (const entry of [...normalizeArray(explicitEntries), ...normalizeArray(generatedEntries)]) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      continue;
    }
    const name = entry.name ? String(entry.name) : '';
    if (name && seen.has(name)) {
      continue;
    }
    if (name) {
      seen.add(name);
    }
    entries.push(entry);
  }
  return entries.length > 0 ? entries : undefined;
}

function sanitizeRecipeName(value) {
  return String(value || '').trim().replace(/[^A-Za-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '');
}

function agentBundleConfigFromAgentTaskRequest(request, config, inputs) {
  const explicitBundleSources = [
    inputs.agent_bundle,
    inputs.agentBundle,
    config.agent_bundle,
    config.agentBundle,
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
  const publicEnvelope = codeboxPublicResultEnvelope(result);
  if (!publicEnvelope && privateCodeboxRuntimeResultShapeNames(result).length > 0) {
    return 'failed';
  }
  if (recipeRunFailedPhase(recipeRunFromResult(result))) {
    return 'failed';
  }
  if (publicEnvelope?.outputs && typeof publicEnvelope.outputs === 'object' && publicEnvelope.outputs.success === false) {
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
  if (publicEnvelope?.success === true && publicEnvelope?.metadata?.no_op_reason) {
    return 'no_op';
  }
  if (result?.outcome === 'no_op' || result?.no_op) {
    return 'no_op';
  }
  if (agentRuntimeSucceeded(result)) {
    return 'succeeded';
  }
  return (publicEnvelope?.success ?? result?.success) === true ? 'succeeded' : 'failed';
}

function agentRuntimeResultCandidates(result) {
  const publicEnvelope = codeboxPublicResultEnvelope(result);
  return [
    publicEnvelope,
    publicEnvelope?.outputs,
    ...(Array.isArray(publicEnvelope?.diagnostics) ? publicEnvelope.diagnostics : []),
  ].filter((candidate) => candidate && typeof candidate === 'object' && !Array.isArray(candidate));
}

function agentRuntimeFailure(result) {
  return agentRuntimeResultCandidates(result).find(agentRuntimeCandidateFailed) || null;
}

function agentRuntimeSucceeded(result) {
  return agentRuntimeResultCandidates(result).some(agentRuntimeCandidateSucceeded);
}

function agentRuntimeCandidateSucceeded(candidate) {
  const runtime = candidate.agent_runtime || candidate.agentRuntime;
  return runtime && typeof runtime === 'object' && !Array.isArray(runtime) && runtime.success === true;
}

function agentRuntimeCandidateFailed(candidate) {
  const runtime = candidate.agent_runtime || candidate.agentRuntime;
  const terminalStatus = String(candidate.terminal_status || candidate.terminalStatus || '').toLowerCase();
  const status = String(candidate.status || candidate.outcome || '').toLowerCase();
  const completionStatus = String(candidate.completion_outcome?.status || candidate.completionOutcome?.status || '').toLowerCase();
  return candidate.success === false
    || runtime?.success === false
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

function typedArtifactsFromResult(result, options = {}) {
  return typedArtifactsFromCodeboxResult(result, { sanitize: sanitizePublicMetadata, ...options });
}

function codeboxAgentResultFromResult(result) {
  return codeboxPublicResultEnvelope(result)?.metadata || {};
}

function codeboxBundleDirectoryFromResult(result) {
  const artifactBundle = codeboxPublicResultEnvelope(result)?.artifact_result?.artifactBundle;
  return artifactBundle?.path || artifactBundle?.uri || '';
}

function firstTranscriptArtifactRefFromResult(result) {
  const publicEnvelope = codeboxPublicResultEnvelope(result);
  const candidates = [
    ...(publicEnvelope?.artifact_result?.artifactRefs || []),
    ...(publicEnvelope?.artifact_result?.evidenceRefs || []),
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
  return codeboxTaskArtifactDeclarations(artifactDeclarationsFromAgentTaskRequest(request, config, request.inputs || {}))
    .filter((declaration) => declaration && typeof declaration === 'object' && declaration.required === true && typedArtifactNameFromDeclaration(declaration));
}

function requiredArtifactDeclarationsFromResultTaskInput(result) {
  const taskInput = result?.task_input || result?.taskInput || {};
  return normalizeArray(taskInput.artifact_declarations || taskInput.artifactDeclarations)
    .map((declaration) => wpCodeboxArtifactDeclarationFromHomeboy(declaration))
    .filter((declaration) => declaration && typeof declaration === 'object' && declaration.required === true && typedArtifactNameFromDeclaration(declaration));
}

function requiredArtifactDeclarationsForResult(request, result) {
  const declarations = [
    ...requiredArtifactDeclarationsFromResultTaskInput(result),
    ...requiredArtifactDeclarationsFromRequest(request),
  ];
  const seen = new Set();
  return declarations.filter((declaration) => {
    const key = `${typedArtifactNameFromDeclaration(declaration)}:${declaration.artifact_schema || declaration.artifactSchema || declaration.schema || ''}`;
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
}

function isTranscriptArtifactDeclaration(declaration) {
  const name = typedArtifactNameFromDeclaration(declaration);
  const type = declaration?.type || declaration?.kind || declaration?.artifact_type || declaration?.artifactType || '';
  const schema = declaration?.artifact_schema || declaration?.artifactSchema || declaration?.schema || '';
  return /transcript|conversation|messages/i.test(`${name} ${type} ${schema}`);
}

function replyTextFromResult(result) {
  const publicEnvelope = codeboxPublicResultEnvelope(result) || {};
  const candidates = [
    publicEnvelope.reply,
    publicEnvelope.outputs?.reply,
    publicEnvelope.outputs?.text,
    publicEnvelope.outputs?.content,
  ];
  return candidates.find((candidate) => typeof candidate === 'string' && candidate.trim() !== '') || '';
}

function replyTypedArtifactsFromResult(request, result, existingTypedArtifacts = {}) {
  const reply = replyTextFromResult(result);
  if (!reply) {
    return {};
  }
  return Object.fromEntries(requiredArtifactDeclarationsForResult(request, result)
    .filter((declaration) => !isTranscriptArtifactDeclaration(declaration))
    .map((declaration) => {
      const name = typedArtifactNameFromDeclaration(declaration);
      if (!name || existingTypedArtifacts[name]) {
        return null;
      }
      return [name, normalizeCodeboxTypedArtifactEntry(name, {
        name,
        artifact_id: name,
        kind: declaration.artifact_schema || declaration.artifactSchema || declaration.schema,
        type: declaration.type || declaration.kind || declaration.artifact_type || declaration.artifactType || name,
        artifact_schema: declaration.artifact_schema || declaration.artifactSchema || declaration.schema,
        payload: {
          content: reply,
          format: 'markdown',
        },
        provenance: { source: 'agent_reply' },
      })];
    })
    .filter(Boolean)
    .filter(([, artifact]) => artifact));
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

function invalidRequiredTypedArtifactDiagnostic(request, outputs) {
  const typedArtifacts = outputs?.typed_artifacts && typeof outputs.typed_artifacts === 'object' && !Array.isArray(outputs.typed_artifacts)
    ? outputs.typed_artifacts
    : {};
  const invalid = requiredArtifactDeclarationsFromRequest(request)
    .map((declaration) => typedArtifactNameFromDeclaration(declaration))
    .filter((name) => name && unexecutedWorkspaceToolCallArtifact(typedArtifacts[name]));
  if (invalid.length === 0) {
    return null;
  }
  return {
    class: 'codebox.required_typed_artifacts_invalid',
    message: `WP Codebox agent task produced invalid required typed artifacts: ${invalid.join(', ')}.`,
    data: { reason: 'invalid_required_typed_artifacts', invalid },
  };
}

function unexecutedWorkspaceToolCallArtifact(artifact) {
  if (!artifact || typeof artifact !== 'object') {
    return false;
  }
  const content = String(artifact.payload?.content || '').trim();
  return /^<workspace_[a-z0-9_:-]+(?:\s[^>]*)?\/>$/i.test(content);
}

function outputsWithInputTypedArtifacts(outputs, request) {
  const typedArtifacts = outputs?.typed_artifacts && typeof outputs.typed_artifacts === 'object' && !Array.isArray(outputs.typed_artifacts)
    ? { ...outputs.typed_artifacts }
    : {};
  let added = false;
  for (const declaration of requiredArtifactDeclarationsFromRequest(request)) {
    const name = typedArtifactNameFromDeclaration(declaration);
    if (!name || typedArtifacts[name]) {
      continue;
    }
    const value = request.inputs?.[name];
    if (value === undefined || value === null || value === '') {
      continue;
    }
    typedArtifacts[name] = normalizeTypedArtifactEntry(name, {
      name,
      type: declaration.type || declaration.kind || declaration.artifact_type || declaration.artifactType || name,
      artifact_schema: declaration.artifact_schema || declaration.artifactSchema || declaration.schema,
      payload: typeof value === 'object' ? value : { content: String(value), format: 'text' },
      metadata: { normalized_from: 'request_input' },
    });
    added = true;
  }
  return added ? { ...outputs, typed_artifacts: typedArtifacts } : outputs;
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
  return normalizeCodeboxArtifactOutcomeContract(artifact, rawArtifact, {
    roleAliases: WP_CODEBOX_ROLE_ALIASES,
    sanitize: sanitizePublicMetadata,
  });
}

function pathValue(source, dottedPath) {
  return String(dottedPath || '').split('.').filter(Boolean).reduce((value, key) => (value && typeof value === 'object' ? value[key] : undefined), source);
}

function firstPlainObject(...candidates) {
  return candidates.find((candidate) => candidate && typeof candidate === 'object' && !Array.isArray(candidate));
}

function normalizeOutputs(result, request = null, options = {}) {
  const publicEnvelope = codeboxPublicResultEnvelope(result, options) || {};
  const publicOutputs = publicEnvelope.outputs && typeof publicEnvelope.outputs === 'object' && !Array.isArray(publicEnvelope.outputs)
    ? publicEnvelope.outputs
    : {};
  const typedArtifacts = typedArtifactsFromResult(result, options);
  Object.assign(typedArtifacts, transcriptTypedArtifactsFromCodeboxResult(request, result, typedArtifacts));
  if (request) {
    Object.assign(typedArtifacts, replyTypedArtifactsFromResult(request, result, typedArtifacts));
  }
  for (const [name, artifact] of Object.entries(typedArtifacts)) {
    typedArtifacts[name] = controllerVisibleTypedArtifact(artifact);
  }
  const appendTypedArtifacts = (outputs) => Object.keys(typedArtifacts).length > 0
    ? {
        ...outputs,
        typed_artifacts: {
          ...(outputs.typed_artifacts && typeof outputs.typed_artifacts === 'object' && !Array.isArray(outputs.typed_artifacts) ? outputs.typed_artifacts : {}),
          ...typedArtifacts,
        },
      }
    : outputs;

  const bundle = result.task_input?.agent_bundle || {};
  const configuredOutputs = firstPlainObject(bundle.runtime_output_projections, bundle.runtimeOutputProjections, bundle.engine_data_outputs, bundle.engineDataOutputs) || {};
  if (Object.keys(configuredOutputs).length === 0) {
    return sanitizePublicMetadata(appendTypedArtifacts(publicOutputs));
  }
  const configuredOutputSources = [
    publicEnvelope,
    publicOutputs,
    publicEnvelope.artifact_result?.result,
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
  return sanitizePublicMetadata(appendTypedArtifacts(Object.keys(outputs).length > 0 ? outputs : publicOutputs));
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
    return reconcileRunSummaryWithPublicEnvelope(enrichFailedCodeboxRunSummary(
      options.normalizeAgentTaskRunResult(result, { exitStatus: options.exitStatus ?? 0 }),
      result,
      options,
    ), result, options);
  } catch {
    return null;
  }
}

function reconcileRunSummaryWithPublicEnvelope(runSummary, result = {}, options = {}) {
  const publicEnvelope = codeboxPublicResultEnvelope(result, options);
  if (!runSummary || typeof runSummary !== 'object' || Array.isArray(runSummary) || publicEnvelope?.success !== true || runSummary.status !== 'failed') {
    return runSummary;
  }
  return {
    ...runSummary,
    status: 'succeeded',
    success: true,
    failure_classification: undefined,
    metadata: {
      ...(runSummary.metadata && typeof runSummary.metadata === 'object' && !Array.isArray(runSummary.metadata) ? runSummary.metadata : {}),
      public_envelope_status: publicEnvelope.status,
      public_envelope_success: true,
    },
  };
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

function normalizeArtifacts(result, runSummary = null, recipeSummary = null, options = {}) {
  const normalizedArtifacts = Array.isArray(runSummary?.artifacts)
    ? runSummary.artifacts.map(artifactFromCodeboxArtifact)
    : [];
  const artifactResult = artifactResultEnvelopeFromCodeboxResult(result, options);
  for (const artifact of artifactResult?.artifactRefs || []) {
    appendUniqueArtifact(normalizedArtifacts, artifactFromCodeboxArtifact(artifact));
  }
  if (Array.isArray(recipeSummary?.artifacts)) {
    recipeSummary.artifacts.map(artifactFromCodeboxArtifact).forEach((artifact) => appendUniqueArtifact(normalizedArtifacts, artifact));
  }

  return normalizedArtifacts;
}

function normalizeEvidenceRefs(result, runSummary = null, recipeSummary = null, options = {}) {
  const artifactResult = artifactResultEnvelopeFromCodeboxResult(result, options);
  const evidenceRefs = [
    ...(artifactResult?.artifactRefs || []).map((artifact) => ({
      kind: artifact.kind,
      uri: artifact.uri || artifact.path || artifact.url,
      label: artifact.name || artifact.kind,
    })),
    ...(artifactResult?.evidenceRefs || []).map((ref) => ({
      kind: ref.kind,
      uri: ref.uri || ref.path || ref.url,
      label: ref.name || ref.kind,
    })),
    ...(runSummary?.artifacts || []).map((artifact) => ({
      kind: artifact.kind,
      uri: artifact.path || artifact.url,
      label: artifact.name || artifact.kind,
    })),
    ...(recipeSummary?.artifacts || []).map((artifact) => ({
      kind: artifact.kind,
      uri: artifact.path || artifact.url,
      label: artifact.name || artifact.kind,
    })),
  ];
  return evidenceRefs.map((ref) => agentTaskEvidenceRefFromRef(ref, 'codebox_evidence')).filter((ref) => ref.uri);
}

function codeboxDecisionEvidence(result, runSummary = null, recipeSummary = null, options = {}) {
  const artifactResult = artifactResultEnvelopeFromCodeboxResult(result, options);
  const publicEnvelope = codeboxPublicResultEnvelope(result, options) || {};
  const publicMetadata = publicEnvelope.metadata || {};
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
    changed_files_count: runSummary?.metadata?.changed_files_count ?? publicMetadata.changed_files_count,
    patch_bytes: runSummary?.metadata?.patch_bytes ?? publicMetadata.patch_bytes,
    patch_sha256: runSummary?.metadata?.patch_sha256 || publicMetadata.patch_sha256,
    no_op_reason: runSummary?.metadata?.no_op_reason || publicMetadata.no_op_reason,
    completion_status: runSummary?.metadata?.completion_status || completionOutcome.status,
    completion_next_action: runSummary?.metadata?.completion_next_action || completionOutcome.nextAction,
    confidence: runSummary?.metadata?.confidence || completionOutcome.confidence,
    recipe_pack: recipeRun.pack || recipeRun.recipe_pack || recipeRun.recipePack,
    recipe_name: recipeRun.name || recipeRun.recipe,
    recipe_ref: recipeRun.ref,
    recipe_target_ref: recipeRun.target_ref || recipeRun.targetRef,
    recipe_failed_phase: recipeFailedPhase,
    agent_runtime_failure_reason: agentRuntimeFailureReason(agentRuntimeFailure(result)),
    artifact_result_operation: artifactResult?.operation,
    artifact_result_status: artifactResult?.status,
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

function normalizeCodeboxAgentTaskEvents(request, result = {}, options = {}) {
  assertAgentTaskRequest(request);
  const diagnostics = [];
  const sourceEvents = [];
  const addEvents = (events, source) => {
    for (const event of normalizeArray(events)) {
      if (event && typeof event === 'object' && !Array.isArray(event)) {
        sourceEvents.push({ event, source, index: sourceEvents.length });
      }
    }
  };

  for (const source of codeboxEventSources(result, options)) {
    if (source.kind === 'events') {
      addEvents(source.events, source.source);
      continue;
    }
    if (source.kind === 'json') {
      const parsed = parseEventJsonPayload(source.contents);
      if (parsed) {
        addEvents(eventsFromPayload(parsed), source.source);
      }
      continue;
    }
    if (source.kind === 'file') {
      const payload = readEventPayloadFile(source.path, diagnostics);
      if (payload) {
        addEvents(eventsFromPayload(payload), `file:${source.path}`);
      }
    }
  }

  const sorted = sourceEvents.sort((left, right) => compareCodeboxEventEntries(left, right));
  const events = sorted.map((entry, index) => normalizeCodeboxAgentTaskEvent(request, entry.event, index + 1, entry.source));
  return { events, diagnostics };
}

function codeboxEventSources(result = {}, options = {}) {
  const publicEnvelope = options.publicResultEnvelope || codeboxPublicResultEnvelope(result, options) || {};
  const sources = [
    { kind: 'events', source: 'options.events', events: options.events },
    { kind: 'events', source: 'result.events', events: result.events },
    { kind: 'events', source: 'result.normalized_events', events: result.normalized_events || result.normalizedEvents },
    { kind: 'events', source: 'result.agent_task_events', events: result.agent_task_events || result.agentTaskEvents },
    { kind: 'events', source: 'result.callback_events', events: result.callback_events || result.callbackEvents },
    { kind: 'events', source: 'result.outputs.callback_events', events: result.outputs?.callback_events || result.outputs?.callbackEvents },
    { kind: 'events', source: 'result.metadata.callback_events', events: result.metadata?.callback_events || result.metadata?.callbackEvents },
    { kind: 'events', source: 'public.outputs.callback_events', events: publicEnvelope.outputs?.callback_events || publicEnvelope.outputs?.callbackEvents },
    { kind: 'events', source: 'public.metadata.callback_events', events: publicEnvelope.metadata?.callback_events || publicEnvelope.metadata?.callbackEvents },
    { kind: 'json', source: 'options.stdout', contents: options.stdout },
    { kind: 'json', source: 'result.stdout', contents: result.stdout },
  ];
  for (const execution of resultExecutionsFromPayload(result)) {
    sources.push({ kind: 'json', source: 'execution.stdout', contents: execution.stdout });
  }
  for (const filePath of codeboxEventFileCandidates(result, options)) {
    sources.push({ kind: 'file', path: filePath });
  }
  return sources;
}

function codeboxEventFileCandidates(result = {}, options = {}) {
  const configured = [
    options.eventsFile,
    options.events_file,
    options.eventFile,
    options.event_file,
    options.resultFile,
    options.result_file,
    result.events_file,
    result.eventsFile,
    result.events_path,
    result.eventsPath,
    result.event_file,
    result.eventFile,
    result.event_path,
    result.eventPath,
    result.result_file,
    result.resultFile,
    result.result_path,
    result.resultPath,
    result.metadata?.events_file,
    result.metadata?.eventsFile,
    result.metadata?.events_path,
    result.metadata?.eventsPath,
    result.metadata?.result_file,
    result.metadata?.resultFile,
  ];
  const refs = [
    ...normalizeArray(result.artifacts),
    ...normalizeArray(result.evidence_refs),
    ...normalizeArray(result.evidenceRefs),
    ...normalizeArray(result.artifact_result?.artifact_refs),
    ...normalizeArray(result.artifact_result?.artifactRefs),
    ...normalizeArray(result.artifact_result?.evidence_refs),
    ...normalizeArray(result.artifact_result?.evidenceRefs),
  ];
  for (const ref of refs) {
    const candidate = ref?.path || ref?.file || ref?.uri;
    if (candidate && /(?:event|result).+\.json$|\.events\.json$|events\.json$|result\.json$/i.test(String(candidate))) {
      configured.push(candidate);
    }
  }
  return uniqueStrings(configured.filter((candidate) => typeof candidate === 'string' && candidate !== ''));
}

function readEventPayloadFile(filePath, diagnostics) {
  if (!fs.existsSync(filePath)) {
    diagnostics.push({
      class: 'codebox.events_file_missing',
      message: `WP Codebox event/result file was not found: ${filePath}.`,
      data: { path: filePath },
    });
    return null;
  }
  try {
    const parsed = readJsonFile(filePath);
    if (parsed !== null) {
      return parsed;
    }
  } catch {
  }
  diagnostics.push({
    class: 'codebox.events_file_invalid_json',
    message: `WP Codebox event/result file did not contain valid JSON: ${filePath}.`,
    data: { path: filePath },
  });
  return null;
}

function eventsFromPayload(payload) {
  if (Array.isArray(payload)) {
    return payload;
  }
  if (!payload || typeof payload !== 'object') {
    return [];
  }
  return [
    ...normalizeArray(payload.events),
    ...normalizeArray(payload.normalized_events || payload.normalizedEvents),
    ...normalizeArray(payload.agent_task_events || payload.agentTaskEvents),
    ...normalizeArray(payload.callback_events || payload.callbackEvents),
    ...normalizeArray(payload.outputs?.callback_events || payload.outputs?.callbackEvents),
    ...normalizeArray(payload.metadata?.callback_events || payload.metadata?.callbackEvents),
    ...resultExecutionsFromPayload(payload).flatMap((execution) => eventsFromPayload(parseEventJsonPayload(execution.stdout) || {})),
  ];
}

function parseEventJsonPayload(value) {
  if (!value || typeof value !== 'string') {
    return null;
  }
  try {
    const parsed = JSON.parse(value);
    return parsed && typeof parsed === 'object' ? parsed : null;
  } catch {
    return null;
  }
}

function resultExecutionsFromPayload(payload = {}) {
  return [
    ...normalizeArray(payload.executions),
    ...normalizeArray(payload.run?.executions),
  ].filter((execution) => execution && typeof execution === 'object' && !Array.isArray(execution));
}

function compareCodeboxEventEntries(left, right) {
  const leftTime = Date.parse(left.event.created_at || left.event.createdAt || left.event.timestamp || left.event.time || '');
  const rightTime = Date.parse(right.event.created_at || right.event.createdAt || right.event.timestamp || right.event.time || '');
  if (Number.isFinite(leftTime) && Number.isFinite(rightTime) && leftTime !== rightTime) {
    return leftTime - rightTime;
  }
  if (Number.isFinite(leftTime) !== Number.isFinite(rightTime)) {
    return Number.isFinite(leftTime) ? -1 : 1;
  }
  const leftSequence = Number.parseInt(left.event.sequence ?? left.event.seq ?? left.event.order ?? '', 10);
  const rightSequence = Number.parseInt(right.event.sequence ?? right.event.seq ?? right.event.order ?? '', 10);
  if (Number.isFinite(leftSequence) && Number.isFinite(rightSequence) && leftSequence !== rightSequence) {
    return leftSequence - rightSequence;
  }
  const leftWorker = String(left.event.worker_id || left.event.workerId || left.event.worker || left.event.metadata?.worker_id || '');
  const rightWorker = String(right.event.worker_id || right.event.workerId || right.event.worker || right.event.metadata?.worker_id || '');
  if (leftWorker !== rightWorker) {
    return leftWorker.localeCompare(rightWorker);
  }
  return left.index - right.index;
}

function normalizeCodeboxAgentTaskEvent(request, event, sequence, source) {
  const metadata = sanitizePublicMetadata({
    ...(event.metadata && typeof event.metadata === 'object' && !Array.isArray(event.metadata) ? event.metadata : {}),
    source,
    source_schema: event.schema,
    source_sequence: event.sequence ?? event.seq ?? event.order,
  });
  const artifactRefs = normalizeArray(event.artifact_refs || event.artifactRefs);
  const artifacts = [
    ...normalizeArray(event.artifacts),
    ...artifactRefs,
  ];
  return withoutEmptyObjectValues({
    schema: AGENT_TASK_EVENT_SCHEMA,
    event_id: event.event_id || event.eventId || event.id || `${request.task_id}:${sequence}`,
    task_id: event.task_id || event.taskId || request.task_id,
    parent_task_id: event.parent_task_id || event.parentTaskId || request.parent_task_id,
    sequence,
    type: event.type || event.name || event.event || event.action || event.status || 'codebox.event',
    name: event.name,
    status: event.status,
    message: event.message || event.summary,
    created_at: event.created_at || event.createdAt || event.timestamp || event.time || new Date(0).toISOString(),
    worker_id: event.worker_id || event.workerId || event.worker || event.metadata?.worker_id,
    group_key: event.group_key || event.groupKey || request.group_key,
    artifacts: artifacts.map((artifact, index) => artifactFromCodeboxArtifact(artifact, index)).filter(Boolean),
    evidence_refs: normalizeArray(event.evidence_refs || event.evidenceRefs).map((ref) => agentTaskEvidenceRefFromRef(ref, 'codebox_event_evidence')).filter((ref) => ref.uri),
    diagnostics: normalizeArray(event.diagnostics).map((diagnostic) => ({
      class: diagnostic.class || diagnostic.kind || diagnostic.code || 'codebox.event',
      message: diagnostic.message || String(diagnostic),
      data: sanitizePublicMetadata(diagnostic.data || {}),
    })),
    metadata,
  });
}

function agentTaskOutcomeFromCodeboxResult(request, result = {}, options = {}) {
  assertAgentTaskRequest(request);
  const publicResultEnvelope = codeboxPublicResultEnvelope(result, options);
  const envelopeOptions = { ...options, publicResultEnvelope };
  const dispatchIdentity = agentTaskDispatchIdentityPassthrough(request, result);
  const normalizedEventEnvelope = normalizeCodeboxAgentTaskEvents(request, result, envelopeOptions);
  const runSummary = codeboxRunSummary(result, envelopeOptions);
  const recipeSummary = codeboxRecipeRunSummary(result, envelopeOptions);
  const localStatus = normalizeStatus(result, options.exitStatus ?? 0);
  let status = runSummary?.status || recipeSummary?.status || localStatus;
  if (recipeSummary?.status && recipeSummary.status !== 'succeeded') {
    status = recipeSummary.status;
  } else if (localStatus === 'failed') {
    status = localStatus;
  }
  const failureClassification = homeboyFailureClassification(result.failure_classification || recipeSummary?.metadata?.failure_classification || runSummary?.failure_classification, status);
  const outputs = outputsWithInputTypedArtifacts(normalizeOutputs(result, request, envelopeOptions), request);
  const missingRequiredTypedArtifacts = missingRequiredTypedArtifactDiagnostic(request, outputs);
  const invalidRequiredTypedArtifacts = invalidRequiredTypedArtifactDiagnostic(request, outputs);
  const runtimeFailureDiagnostic = agentRuntimeFailureDiagnostic(result);
  const envelopeBoundaryDiagnostic = publicEnvelopeBoundaryDiagnostic(result, envelopeOptions);
  if (status === 'succeeded' && missingRequiredTypedArtifacts) {
    status = 'failed';
  }
  if (status === 'succeeded' && invalidRequiredTypedArtifacts) {
    status = 'failed';
  }
  if (status === 'succeeded' && runtimeFailureDiagnostic) {
    status = 'failed';
  }
  if (envelopeBoundaryDiagnostic) {
    status = 'failed';
  }
  const recipeRun = recipeRunFromResult(result);
  const fallbackRecipeSummary = recipeRunFailureSummary(recipeRun);
  const recipeFailedPhase = recipeSummary?.failed_phase || recipeSummary?.metadata?.failure_phase || recipeRunFailedPhase(recipeRun);
  const providerDiagnostic = providerNotRegisteredDiagnostic(request, result);
  const outcome = normalizeAgentTaskOutcome(request, result, {
    schema: AGENT_TASK_OUTCOME_SCHEMA,
    provider: 'wordpress.codebox-agent-task-executor',
    providerLabel: 'WP Codebox agent',
    integrationContract: WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA,
    status,
    summary: envelopeBoundaryDiagnostic?.message || missingRequiredTypedArtifacts?.message || invalidRequiredTypedArtifacts?.message || runtimeFailureDiagnostic?.message || recipeSummary?.failure_summary || fallbackRecipeSummary || runSummary?.summary || result.summary || result.message || (status === 'succeeded' ? 'WP Codebox agent task succeeded.' : 'WP Codebox agent task failed.'),
    artifacts: normalizeArtifacts(result, runSummary, recipeSummary, envelopeOptions),
    evidenceRefs: normalizeEvidenceRefs(result, runSummary, recipeSummary, envelopeOptions),
    outputs,
    diagnostics: [providerDiagnostic, envelopeBoundaryDiagnostic, missingRequiredTypedArtifacts, invalidRequiredTypedArtifacts, runtimeFailureDiagnostic, recipeSummary ? null : recipeRunFailureDiagnostic(recipeRun), ...normalizedEventEnvelope.diagnostics, ...(recipeSummary?.diagnostics || []), ...(runSummary?.diagnostics || []), ...(result.diagnostics || [])].filter(Boolean).map((diagnostic) => ({
      class: diagnostic.class || diagnostic.kind || 'codebox',
      message: diagnostic.message || String(diagnostic),
      data: sanitizePublicMetadata(diagnostic.data || {}),
    })),
    metadata: {
      codebox: sanitizePublicMetadata(result.metadata || result),
      codebox_run_result: runSummary ? sanitizePublicMetadata(runSummary) : undefined,
      codebox_recipe_run_summary: recipeSummary ? sanitizePublicMetadata(recipeSummary) : undefined,
      decision_evidence: sanitizePublicMetadata(codeboxDecisionEvidence(result, runSummary, recipeSummary, envelopeOptions)),
      artifact_declarations: sanitizePublicMetadata(artifactDeclarationsMetadataFromRequest(request)),
      typed_artifacts: sanitizePublicMetadata(outputs.typed_artifacts || {}),
      dispatch_identity: dispatchIdentity ? sanitizePublicMetadata(dispatchIdentity) : undefined,
      normalized_events: sanitizePublicMetadata(normalizedEventEnvelope.events),
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
  } else if (envelopeBoundaryDiagnostic || missingRequiredTypedArtifacts || invalidRequiredTypedArtifacts) {
    outcome.failure_classification = 'execution_failed';
  }
  return outcomeWithOutputTypedArtifacts(outcomeWithNormalizedEvents(outcome, normalizedEventEnvelope.events), outputs);
}

function agentTaskDispatchIdentityPassthrough(request = {}, result = {}) {
  return firstObject(
    request.dispatch_identity,
    request.dispatchIdentity,
    request.inputs?.dispatch_identity,
    request.inputs?.dispatchIdentity,
    request.inputs?.input?.dispatch_identity,
    request.inputs?.input?.dispatchIdentity,
    request.inputs?.input?.metadata?.dispatch_identity,
    request.inputs?.input?.metadata?.dispatchIdentity,
    request.executor?.config?.runtime_task?.input?.dispatch_identity,
    request.executor?.config?.runtime_task?.input?.dispatchIdentity,
    request.executor?.config?.runtime_task?.input?.metadata?.dispatch_identity,
    request.executor?.config?.runtime_task?.input?.metadata?.dispatchIdentity,
    result.dispatch_identity,
    result.dispatchIdentity,
    result.metadata?.dispatch_identity,
    result.metadata?.dispatchIdentity,
    result.task_input?.dispatch_identity,
    result.task_input?.dispatchIdentity,
    result.task_input?.runtime_task?.input?.dispatch_identity,
    result.task_input?.runtime_task?.input?.dispatchIdentity,
    result.task_input?.runtime_task?.input?.metadata?.dispatch_identity,
    result.task_input?.runtime_task?.input?.metadata?.dispatchIdentity,
  );
}

function outcomeWithNormalizedEvents(outcome, events) {
  if (!Array.isArray(events) || events.length === 0) {
    return outcome;
  }
  return {
    ...outcome,
    events,
  };
}

function outcomeWithOutputTypedArtifacts(outcome, outputs) {
  const typedArtifacts = outputs?.typed_artifacts && typeof outputs.typed_artifacts === 'object' && !Array.isArray(outputs.typed_artifacts)
    ? Object.values(outputs.typed_artifacts).filter((artifact) => artifact && typeof artifact === 'object').map(controllerVisibleTypedArtifact)
    : [];
  if (typedArtifacts.length === 0) {
    return outcome;
  }
  const existing = Array.isArray(outcome.typed_artifacts) ? outcome.typed_artifacts : [];
  const seen = new Set(existing.map((artifact) => artifact?.name).filter(Boolean));
  const merged = [
    ...existing,
    ...typedArtifacts.filter((artifact) => {
      if (!artifact.name || seen.has(artifact.name)) {
        return false;
      }
      seen.add(artifact.name);
      return true;
    }),
  ];
  return {
    ...outcome,
    typed_artifacts: merged,
  };
}

function controllerVisibleTypedArtifact(artifact) {
  const artifactId = artifact.artifact_id || artifact.artifactId || artifact.name || artifact.id;
  const kind = artifact.kind || artifact.artifact_schema || artifact.artifactSchema || artifact.type;
  return {
    ...artifact,
    ...(artifactId ? { artifact_id: artifactId } : {}),
    ...(kind ? { kind } : {}),
  };
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
  WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA,
  RUNTIME_EXECUTION_DESCRIPTOR_SCHEMA,
  AGENT_TASK_EVENT_SCHEMA,
  providerContract,
  providerRuntimeInvocationContract,
  wpCodeboxAgentFanoutAdapterContract,
  codeboxTaskRequestFromAgentTaskRequest,
  codeboxFanoutRequestFromAgentTaskRequest,
  reconcileRunSummaryWithPublicEnvelope,
  normalizeCodeboxAgentTaskEvents,
  agentTaskOutcomeFromCodeboxResult,
};
