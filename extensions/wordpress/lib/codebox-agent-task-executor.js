'use strict';

const AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const AGENT_TASK_OUTCOME_SCHEMA = 'homeboy/agent-task-outcome/v1';
const AGENT_TASK_ARTIFACT_SCHEMA = 'homeboy/agent-task-artifact/v1';
const WP_CODEBOX_TASK_REQUEST_SCHEMA = 'homeboy/wp-codebox-task-request/v1';

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
  'datamachine_bundle_execution',
];

const DATAMACHINE_BUNDLE_CONFIG_FIELDS = [
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

const WP_CODEBOX_RUNTIME_GAP_TRACKERS = [
  'https://github.com/Automattic/wp-codebox/issues/529',
  'https://github.com/Automattic/wp-codebox/issues/530',
  'https://github.com/Automattic/wp-codebox/issues/531',
  'https://github.com/Automattic/wp-codebox/issues/532',
];

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
    status: 'preparatory',
    upstream_dependency: 'https://github.com/Automattic/wp-codebox/issues/480',
    runtime_gap_trackers: WP_CODEBOX_RUNTIME_GAP_TRACKERS,
  };
}

function codeboxTaskRequestFromAgentTaskRequest(request, options = {}) {
  assertAgentTaskRequest(request);
  const config = request.executor.config || {};
  const inputs = request.inputs || {};
  const executionKind = config.execution_kind || config.executionKind || config.kind || options.executionKind || 'sandbox';
  const datamachineBundle = datamachineBundleConfigFromAgentTaskRequest(request, config, inputs);
  const timeoutSeconds = request.limits?.task_timeout_seconds || request.limits?.taskTimeoutSeconds;
  const timeoutMs = request.limits?.timeout_ms || request.limits?.max_runtime_ms;
  const timeoutFromMs = timeoutMs ? Math.ceil(timeoutMs / 1000) : undefined;

  return {
    schema: WP_CODEBOX_TASK_REQUEST_SCHEMA,
    sandbox_session_id: config.sandbox_session_id || request.task_id,
    group_key: request.group_key,
    execution_kind: executionKind,
    agent: config.agent || options.agent || 'wp-codebox-sandbox',
    mode: config.mode || options.mode || 'sandbox',
    provider: config.provider || options.provider || '',
    model: request.executor.model || config.model || options.model || '',
    provider_plugin_paths: config.provider_plugin_paths || options.providerPluginPaths || [],
    runtime_stack_mounts: config.runtime_stack_mounts || options.runtimeStackMounts || [],
    runtime_overlays: config.runtime_overlays || options.runtimeOverlays || [],
    secret_env: config.secret_env || options.secretEnv || [],
    agents_api: config.agents_api || options.agentsApi || '',
    data_machine: config.data_machine || options.dataMachine || '',
    data_machine_code: config.data_machine_code || options.dataMachineCode || '',
    homeboy: config.homeboy || options.homeboy || '',
    homeboy_extensions: config.homeboy_extensions || options.homeboyExtensions || '',
    wp_cli_bin: config.wp_cli_bin || options.wpCliBin || '',
    wp_codebox_bin: config.wp_codebox_bin || options.wpCodeboxBin || '',
    wp_codebox_wordpress_version: config.wp_codebox_wordpress_version || config.wpCodeboxWordpressVersion || options.wpCodeboxWordpressVersion || '',
    artifacts: config.artifacts || options.artifacts || '',
    max_turns: config.max_turns || options.maxTurns,
    task_timeout_seconds: config.task_timeout_seconds || timeoutSeconds || timeoutFromMs || options.taskTimeoutSeconds,
    datamachine_bundle: datamachineBundle,
    orchestrator: {
      ...(inputs.orchestrator || {}),
      agent_task_id: request.task_id,
      parent_plan_id: request.parent_plan_id,
      source_refs: request.source_refs || [],
    },
    audit_findings: inputs.audit_findings || [],
    task: {
      title: inputs.title || request.metadata?.title || `Run Codebox agent task ${request.task_id}`,
      prompt: request.instructions,
      expected_artifacts: request.expected_artifacts || [],
      policy: request.policy || {},
      workspace: request.workspace || {},
      inputs,
    },
  };
}

function datamachineBundleConfigFromAgentTaskRequest(request, config, inputs) {
  const candidateSources = [
    inputs.datamachine_bundle,
    inputs.datamachineBundle,
    inputs,
    config.datamachine_bundle,
    config.datamachineBundle,
    config,
  ].filter((value) => value && typeof value === 'object');
  const bundleConfig = {};
  for (const field of DATAMACHINE_BUNDLE_CONFIG_FIELDS) {
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
  if (result?.status) {
    return result.status;
  }
  const agentResult = result?.run?.agentResult || result?.agentResult || result?.metadata?.recipe_run?.agentResult || result?.metadata?.recipe_run?.run?.agentResult;
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

  const bundleDirectory = result.run?.agentResult?.artifacts?.directory || result.completionOutcome?.provenance?.artifactDirectory;
  const artifactBundleId = result.completionOutcome?.provenance?.artifactBundleId || result.artifacts?.id;
  appendUniqueArtifact(artifacts, {
    id: artifactBundleId,
    kind: 'codebox-artifact-bundle',
    path: bundleDirectory,
    metadata: {
      runtime_id: result.run?.runtime?.id,
      runtime_status: result.run?.runtime?.status,
    },
  });

  const agentResult = result.run?.agentResult || result.agentResult || result.metadata?.recipe_run?.agentResult || {};
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
    for (const artifact of datamachineBundleArtifacts(result)) {
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
    for (const artifact of datamachineBundleArtifacts(result)) {
      appendUniqueEvidenceRef(refs, {
        kind: artifact.kind,
        uri: artifact.path || artifact.url,
        label: artifact.kind.replace(/^datamachine-/, 'Data Machine ').replace(/-/g, ' '),
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

function datamachineBundleArtifacts(result) {
  const artifacts = [];
  const workload = result.metadata?.datamachine?.workload || result.run?.agentResult || result.agentResult || {};
  const scenarios = Array.isArray(workload.scenarios) ? workload.scenarios : [];
  for (const scenario of scenarios) {
    const metadata = scenario?.metadata || {};
    const transcript = metadata.transcript_artifacts || {};
    appendUniqueArtifact(artifacts, {
      id: transcript.json ? 'datamachine-transcript-json' : '',
      kind: 'datamachine-transcript',
      path: transcript.json,
      metadata: { scenario_id: scenario.id, format: 'json' },
    });
    appendUniqueArtifact(artifacts, {
      id: transcript.summary ? 'datamachine-transcript-summary' : '',
      kind: 'datamachine-transcript-summary',
      path: transcript.summary,
      metadata: { scenario_id: scenario.id, format: 'markdown' },
    });
    const replayBundlePath = metadata.replay_bundle_path || metadata.replay_bundle?.path;
    appendUniqueArtifact(artifacts, {
      id: replayBundlePath ? 'datamachine-replay-bundle' : '',
      kind: 'datamachine-replay-bundle',
      path: replayBundlePath,
      metadata: { scenario_id: scenario.id },
    });
    const exports = Array.isArray(metadata.job_artifact_exports) ? metadata.job_artifact_exports : [];
    for (const [index, exported] of exports.entries()) {
      appendUniqueArtifact(artifacts, {
        id: exported.id || exported.path || `datamachine-job-artifact-${index + 1}`,
        kind: exported.kind || 'datamachine-job-artifact',
        path: exported.path,
        url: exported.url,
        metadata: { ...exported, scenario_id: scenario.id },
      });
    }
    const pullRequestUrl = metadata.fallback_pull_request?.url || metadata.engine_data?.pull_request?.url || metadata.engine_data?.static_site_agent?.pr_url;
    appendUniqueArtifact(artifacts, {
      id: pullRequestUrl ? 'datamachine-pull-request' : '',
      kind: 'datamachine-pull-request',
      url: pullRequestUrl,
      metadata: { scenario_id: scenario.id },
    });
  }
  return artifacts;
}

function codeboxDecisionEvidence(result) {
  const agentResult = result.run?.agentResult || result.agentResult || result.metadata?.recipe_run?.agentResult || result.metadata?.recipe_run?.run?.agentResult || {};
  const completionOutcome = result.completionOutcome || result.metadata?.recipe_run?.completionOutcome || {};
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
  const outcome = {
    schema: AGENT_TASK_OUTCOME_SCHEMA,
    task_id: request.task_id,
    status,
    summary: result.summary || result.message || (status === 'succeeded' ? 'WP Codebox agent task succeeded.' : 'WP Codebox agent task failed.'),
    artifacts: normalizeArtifacts(result),
    evidence_refs: normalizeEvidenceRefs(result),
    diagnostics: (result.diagnostics || []).map((diagnostic) => ({
      class: diagnostic.class || diagnostic.kind || 'codebox',
      message: diagnostic.message || String(diagnostic),
      data: sanitizePublicMetadata(diagnostic.data || {}),
    })),
    metadata: {
      provider: 'wordpress.codebox-agent-task-executor',
      codebox: sanitizePublicMetadata(result.metadata || result),
      upstream_dependency: 'https://github.com/Automattic/wp-codebox/issues/480',
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
