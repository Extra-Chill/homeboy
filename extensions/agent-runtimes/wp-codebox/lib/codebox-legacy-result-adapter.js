'use strict';

const {
  runtimeContractSchemas,
} = require('./wp-codebox-runtime-contract-source');

const WP_CODEBOX_AGENT_TASK_RUN_RESPONSE_SCHEMA = runtimeContractSchemas().agentTask.legacyRunResponse;

const DEPRECATED_CODEBOX_LEGACY_RESULT_ADAPTER = {
  status: 'deprecated',
  replacement: 'wp-codebox/agent-task-run-result/v1 and wp-codebox/artifact-result-envelope/v1',
  reason: 'Compatibility with older WP Codebox agent-task-run responses and runtime-local artifact layouts.',
};

function isCodeboxLegacyAgentTaskRunResult(result) {
  return result?.schema === WP_CODEBOX_AGENT_TASK_RUN_RESPONSE_SCHEMA;
}

function legacyAgentTaskRunSessionArtifacts(result = {}) {
  if (!isCodeboxLegacyAgentTaskRunResult(result)) {
    return [];
  }
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
  return artifacts;
}

function legacyAgentTaskRunEvidenceRefs(result = {}) {
  if (!isCodeboxLegacyAgentTaskRunResult(result)) {
    return [];
  }
  return [
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
}

function legacyArtifactResultEnvelopeCandidates(result = {}) {
  return [
    result?.metadata?.agent_runtime?.result,
  ];
}

function legacyTypedArtifactCandidatesFromCodeboxResult(result, workload = {}) {
  const scenarios = Array.isArray(workload.scenarios) ? workload.scenarios : [];
  return [
    result?.outputs?.typed_artifacts,
    result?.outputs?.typedArtifacts,
    result?.run?.agentResult?.typed_artifacts,
    result?.run?.agentResult?.typedArtifacts,
    result?.run?.agentResult?.outputs?.typed_artifacts,
    result?.run?.agentResult?.outputs?.typedArtifacts,
    result?.agentResult?.outputs?.typed_artifacts,
    result?.agentResult?.outputs?.typedArtifacts,
    result?.agent_result?.outputs?.typed_artifacts,
    result?.agent_result?.outputs?.typedArtifacts,
    result?.metadata?.agent_runtime?.result?.typed_artifacts,
    result?.metadata?.agent_runtime?.result?.typedArtifacts,
    result?.metadata?.agent_runtime?.result?.outputs?.typed_artifacts,
    result?.metadata?.agent_runtime?.result?.outputs?.typedArtifacts,
    result?.metadata?.agent_runtime?.result?.outputs?.outputs?.typed_artifacts,
    result?.metadata?.agent_runtime?.result?.outputs?.outputs?.typedArtifacts,
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
    ...scenarios.map((scenario) => scenario?.metadata?.engine_data?.outputs?.typed_artifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.engine_data?.outputs?.typedArtifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.outputs?.typed_artifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.outputs?.typedArtifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.typed_artifacts),
    ...scenarios.map((scenario) => scenario?.metadata?.typedArtifacts),
  ];
}

module.exports = {
  DEPRECATED_CODEBOX_LEGACY_RESULT_ADAPTER,
  isCodeboxLegacyAgentTaskRunResult,
  legacyAgentTaskRunEvidenceRefs,
  legacyAgentTaskRunSessionArtifacts,
  legacyArtifactResultEnvelopeCandidates,
  legacyTypedArtifactCandidatesFromCodeboxResult,
};
