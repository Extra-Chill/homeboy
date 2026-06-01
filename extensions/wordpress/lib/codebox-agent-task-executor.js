'use strict';

const AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const AGENT_TASK_OUTCOME_SCHEMA = 'homeboy/agent-task-outcome/v1';
const AGENT_TASK_ARTIFACT_SCHEMA = 'homeboy/agent-task-artifact/v1';
const WP_CODEBOX_TASK_REQUEST_SCHEMA = 'homeboy/wp-codebox-task-request/v1';

const PROVIDER_CAPABILITIES = [
  'browser_runtime',
  'wordpress_sandbox',
  'artifact_materialization',
  'screenshots',
  'structured_outcome',
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
    capabilities: PROVIDER_CAPABILITIES,
    status: 'preparatory',
    upstream_dependency: 'https://github.com/chubes4/wp-codebox/issues/392',
  };
}

function codeboxTaskRequestFromAgentTaskRequest(request, options = {}) {
  assertAgentTaskRequest(request);
  const config = request.executor.config || {};
  const inputs = request.inputs || {};
  const timeoutMs = request.limits?.timeout_ms || request.limits?.max_runtime_ms;
  const timeoutSeconds = timeoutMs ? Math.ceil(timeoutMs / 1000) : undefined;

  return {
    schema: WP_CODEBOX_TASK_REQUEST_SCHEMA,
    sandbox_session_id: config.sandbox_session_id || request.task_id,
    group_key: request.group_key,
    provider: config.provider || options.provider || '',
    model: request.executor.model || config.model || options.model || '',
    provider_plugin_paths: config.provider_plugin_paths || options.providerPluginPaths || [],
    secret_env: config.secret_env || options.secretEnv || [],
    max_turns: config.max_turns || options.maxTurns,
    task_timeout_seconds: config.task_timeout_seconds || timeoutSeconds || options.taskTimeoutSeconds,
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

function normalizeStatus(result, exitStatus = 0) {
  if (result?.status) {
    return result.status;
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
    if (/^(secret_env_values|secretEnvValues|secrets)$/i.test(key)) {
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
  const artifacts = Array.isArray(result?.artifacts)
    ? result.artifacts
    : Object.values(result?.artifacts || {}).filter((value) => value && typeof value === 'object');
  return artifacts.map(artifactFromCodeboxArtifact);
}

function normalizeEvidenceRefs(result) {
  const evidenceRefs = result?.evidence_refs || result?.evidence || [];
  return evidenceRefs.map((ref) => ({
    kind: ref.kind || ref.type || 'codebox_evidence',
    uri: ref.uri || ref.url || ref.path,
    label: ref.label || ref.name,
  })).filter((ref) => ref.uri);
}

function agentTaskOutcomeFromCodeboxResult(request, result = {}, options = {}) {
  assertAgentTaskRequest(request);
  const status = normalizeStatus(result, options.exitStatus ?? 0);
  const failureClassification = failureClassificationForStatus(status);
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
      data: diagnostic.data || {},
    })),
    metadata: {
      provider: 'wordpress.codebox-agent-task-executor',
      codebox: sanitizePublicMetadata(result.metadata || result),
      upstream_dependency: 'https://github.com/chubes4/wp-codebox/issues/392',
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
  providerContract,
  codeboxTaskRequestFromAgentTaskRequest,
  agentTaskOutcomeFromCodeboxResult,
};
