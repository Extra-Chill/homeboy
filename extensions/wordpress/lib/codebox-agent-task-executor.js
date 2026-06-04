'use strict';

const AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const AGENT_TASK_OUTCOME_SCHEMA = 'homeboy/agent-task-outcome/v1';
const AGENT_TASK_ARTIFACT_SCHEMA = 'homeboy/agent-task-artifact/v1';
const WP_CODEBOX_TASK_REQUEST_SCHEMA = 'wp-codebox/task-input/v1';

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
];

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
  'fallback_pull_request',
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
const WP_CODEBOX_RUNTIME_PATH_KEY = `${LEGACY_RUNTIME_PREFIX}_path`;
const WP_CODEBOX_RUNTIME_TOOLS_PATH_KEY = `${LEGACY_RUNTIME_PREFIX}_code_path`;
const LEGACY_BUNDLE_KEYS = [
  `${LEGACY_RUNTIME_PREFIX}_bundle`,
  `${LEGACY_RUNTIME_PREFIX}Bundle`,
];

const WP_CODEBOX_RUNTIME_GAP_TRACKERS = [];

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
  if (!request.executor || request.executor.backend !== 'codebox') {
    throw new Error('Codebox executor provider only accepts executor.backend "codebox".');
  }
}

function providerContract(options = {}) {
  return {
    schema: 'homeboy/agent-task-executor-provider/v1',
    id: options.id || 'wordpress.codebox-agent-task-executor',
    label: options.label || 'WP Codebox agent task executor',
    backend: 'codebox',
    command: options.command || 'node {{extension_path}}/scripts/agent/homeboy-codebox-agent-task-executor.cjs',
    request_schema: AGENT_TASK_REQUEST_SCHEMA,
    outcome_schema: AGENT_TASK_OUTCOME_SCHEMA,
    request_required_fields: ['schema', 'task_id', 'executor.backend', 'instructions'],
    outcome_statuses: AGENT_TASK_OUTCOME_STATUSES,
    failure_classifications: AGENT_TASK_FAILURE_CLASSIFICATIONS,
    redacted_metadata_keys: AGENT_TASK_REDACTED_METADATA_KEYS,
    capabilities: PROVIDER_CAPABILITIES,
    status: 'active',
    integration_contract: 'wp-codebox-cli/agent-task-run',
    runtime_gap_trackers: WP_CODEBOX_RUNTIME_GAP_TRACKERS,
  };
}

function codeboxTaskRequestFromAgentTaskRequest(request, options = {}) {
  assertAgentTaskRequest(request);
  const config = request.executor.config || {};
  const inputs = request.inputs || {};
  const agentBundle = agentBundleConfigFromAgentTaskRequest(request, config, inputs);
  const mounts = agentBundleMounts(agentBundle, config.mounts || options.mounts || []);
  const components = runtimeComponentPaths(config, options);
  const timeoutSeconds = request.limits?.task_timeout_seconds || request.limits?.taskTimeoutSeconds;
  const timeoutMs = request.limits?.timeout_ms || request.limits?.max_runtime_ms;
  const timeoutFromMs = timeoutMs ? Math.ceil(timeoutMs / 1000) : undefined;
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
    allowed_tools: inputs.allowed_tools || inputs.allowedTools || [],
    expected_artifacts: request.expected_artifacts || [],
    policy: request.policy || {},
    context,
    sandbox_session_id: config.sandbox_session_id || request.task_id,
    session_id: config.session_id || config.sessionId || '',
    agent: config.agent || options.agent || 'wp-codebox-sandbox',
    mode: config.mode || options.mode || 'sandbox',
    provider: config.provider || options.provider || '',
    model: request.executor.model || config.model || options.model || '',
    provider_plugin_paths: config.provider_plugin_paths || options.providerPluginPaths || [],
    agent_bundles: config.agent_bundles || config.agentBundles || options.agentBundles || [],
    runtime_stack_mounts: config.runtime_stack_mounts || options.runtimeStackMounts || [],
    runtime_overlays: config.runtime_overlays || options.runtimeOverlays || [],
    secret_env: config.secret_env || options.secretEnv || [],
    mounts,
    workspaces: config.workspaces || options.workspaces || [],
    agents_api_path: components.agents_api || config.agents_api || config.agents_api_path || options.agentsApi || '',
    [WP_CODEBOX_RUNTIME_PATH_KEY]: components.agent_runtime || '',
    [WP_CODEBOX_RUNTIME_TOOLS_PATH_KEY]: components.agent_runtime_tools || '',
    runtime_component_paths: components,
    homeboy_path: config.homeboy || config.homeboy_path || options.homeboy || '',
    homeboy_extensions_path: config.homeboy_extensions || config.homeboy_extensions_path || options.homeboyExtensions || '',
    wp_codebox_bin: config.wp_codebox_bin || options.wpCodeboxBin || '',
    wp: config.wp_codebox_wordpress_version || config.wpCodeboxWordpressVersion || options.wpCodeboxWordpressVersion || '',
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

function runtimeComponentPaths(config, options = {}) {
  const explicit = config.runtime_component_paths && typeof config.runtime_component_paths === 'object'
    ? config.runtime_component_paths
    : {};
  const legacyRuntimePath = config[LEGACY_RUNTIME_PREFIX] || config[`${LEGACY_RUNTIME_PREFIX}_path`] || options.legacyRuntime;
  const legacyToolsKey = `${LEGACY_RUNTIME_PREFIX}_code`;
  const legacyRuntimeToolsPath = config[legacyToolsKey] || config[`${legacyToolsKey}_path`] || options.legacyRuntimeTools;
  return Object.fromEntries(Object.entries({
    ...explicit,
    agents_api: explicit.agents_api || config.agents_api || config.agents_api_path || options.agentsApi,
    agent_runtime: explicit.agent_runtime || config.agent_runtime || config.agent_runtime_path || legacyRuntimePath,
    agent_runtime_tools: explicit.agent_runtime_tools || config.agent_runtime_tools || config.agent_runtime_tools_path || legacyRuntimeToolsPath,
  }).filter(([, value]) => value !== undefined && value !== ''));
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
  if (AGENT_TASK_OUTCOME_STATUSES.includes(result?.status)) {
    return result.status;
  }
  if (result?.status === 'completed') {
    return result?.success === true && exitStatus === 0 ? 'succeeded' : 'failed';
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
  return result?.success === true && exitStatus === 0 ? 'succeeded' : 'failed';
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
  const workload = result.metadata?.agent_runtime?.workload || result.run?.agentResult || result.agentResult || result.agent_result || {};
  if (workload.outputs && typeof workload.outputs === 'object' && !Array.isArray(workload.outputs)) {
    return sanitizePublicMetadata(workload.outputs);
  }

  const bundle = result.metadata?.agent_runtime?.bundle || result.task_input?.agent_bundle || {};
  const configuredOutputs = bundle.engine_data_outputs && typeof bundle.engine_data_outputs === 'object' ? bundle.engine_data_outputs : {};
  const scenarios = Array.isArray(workload.scenarios) ? workload.scenarios : [];
  const outputs = {};
  for (const [name, outputPath] of Object.entries(configuredOutputs)) {
    for (const scenario of scenarios) {
      const value = pathValue(scenario, outputPath);
      if (value !== undefined && value !== null && value !== '') {
        outputs[name] = value;
        break;
      }
    }
  }
  return sanitizePublicMetadata(outputs);
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

function normalizeArtifacts(result) {
  if (result?.schema === 'wp-codebox/agent-task-run/v1') {
    const artifacts = [];
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
    for (const artifact of codeboxBundleArtifacts(result)) {
      appendUniqueArtifact(artifacts, artifact);
    }
    for (const artifact of agentRuntimeBundleArtifacts(result)) {
      appendUniqueArtifact(artifacts, artifact);
    }
    return artifacts.map(artifactFromCodeboxArtifact);
  }
  const artifacts = Array.isArray(result?.artifacts)
    ? result.artifacts
    : Object.values(result?.artifacts || {}).filter((value) => value && typeof value === 'object');
  return artifacts.map(artifactFromCodeboxArtifact);
}

function normalizeEvidenceRefs(result) {
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
    for (const ref of outputEvidenceRefs(normalizeOutputs(result))) {
      appendUniqueEvidenceRef(refs, ref);
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
  const workload = result.metadata?.agent_runtime?.workload || result.run?.agentResult || result.agentResult || {};
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
    const pullRequestUrl = metadata.fallback_pull_request?.url || metadata.engine_data?.pull_request?.url || metadata.engine_data?.static_site_agent?.pr_url;
    appendUniqueArtifact(artifacts, {
      id: pullRequestUrl ? 'agent-runtime-pull-request' : '',
      kind: 'agent-runtime-pull-request',
      url: pullRequestUrl,
      metadata: { scenario_id: scenario.id },
    });
  }
  return artifacts;
}

function codeboxDecisionEvidence(result) {
  const agentResult = result.run?.agentResult || result.agentResult || result.agent_result || result.metadata?.recipe_run?.agentResult || result.metadata?.recipe_run?.run?.agentResult || {};
  const completionOutcome = result.completionOutcome || result.completion_outcome || result.metadata?.recipe_run?.completionOutcome || {};
  const runtime = result.run?.runtime || result.metadata?.recipe_run?.run?.runtime || {};
  const run = result.run || result.metadata?.recipe_run?.run || {};
  return Object.fromEntries(Object.entries({
    selected_backend: 'codebox',
    selected_executor: 'wordpress.codebox-agent-task-executor',
    capabilities_used: PROVIDER_CAPABILITIES,
    runtime_gap_trackers: WP_CODEBOX_RUNTIME_GAP_TRACKERS,
    run_id: run.runId,
    run_status: run.status,
    runtime_id: runtime.id,
    runtime_status: runtime.status,
    heartbeat_at: run.heartbeatAt,
    cleanup_observed: runtime.status === 'destroyed' ? 'runtime_destroyed' : '',
    changed_files_count: agentResult.changedFiles?.count,
    patch_bytes: agentResult.patch?.bytes,
    patch_sha256: agentResult.patch?.sha256,
    no_op_reason: agentResult.noOpReason,
    completion_status: completionOutcome.status,
    completion_next_action: completionOutcome.nextAction,
    confidence: completionOutcome.confidence,
  }).filter(([, value]) => value !== undefined && value !== ''));
}

function agentTaskOutcomeFromCodeboxResult(request, result = {}, options = {}) {
  assertAgentTaskRequest(request);
  const status = normalizeStatus(result, options.exitStatus ?? 0);
  const failureClassification = result.failure_classification || failureClassificationForStatus(status);
  const outputs = normalizeOutputs(result);
  const outcome = {
    schema: AGENT_TASK_OUTCOME_SCHEMA,
    task_id: request.task_id,
    status,
    summary: result.summary || result.message || (status === 'succeeded' ? 'WP Codebox agent task succeeded.' : 'WP Codebox agent task failed.'),
    artifacts: normalizeArtifacts(result),
    evidence_refs: normalizeEvidenceRefs(result),
    outputs,
    diagnostics: (result.diagnostics || []).map((diagnostic) => ({
      class: diagnostic.class || diagnostic.kind || 'codebox',
      message: diagnostic.message || String(diagnostic),
      data: sanitizePublicMetadata(diagnostic.data || {}),
    })),
    metadata: {
      provider: 'wordpress.codebox-agent-task-executor',
      codebox: sanitizePublicMetadata(result.metadata || result),
      integration_contract: 'wp-codebox-cli/agent-task-run',
      decision_evidence: sanitizePublicMetadata(codeboxDecisionEvidence(result)),
    },
  };
  if (failureClassification) {
    outcome.failure_classification = failureClassification;
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
  providerContract,
  codeboxTaskRequestFromAgentTaskRequest,
  agentTaskOutcomeFromCodeboxResult,
};
