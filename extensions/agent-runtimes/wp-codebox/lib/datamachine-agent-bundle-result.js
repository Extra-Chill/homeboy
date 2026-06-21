'use strict';

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function nonEmptyObject(value) {
  return plainObject(value) && Object.keys(value).length > 0;
}

function keepMetadataValue(value) {
  if (value === undefined) {
    return false;
  }
  if (Array.isArray(value)) {
    return value.length > 0;
  }
  if (plainObject(value)) {
    return nonEmptyObject(value);
  }
  return true;
}

function normalizeTranscriptArtifacts(bundleRun) {
  const transcripts = bundleRun.transcripts || bundleRun.transcript_artifacts || bundleRun.transcriptArtifacts;
  if (Array.isArray(transcripts)) {
    return transcripts;
  }
  if (plainObject(transcripts)) {
    return transcripts;
  }
  return undefined;
}

function normalizeArtifactExports(bundleRun) {
  const artifacts = bundleRun.artifacts || bundleRun.artifact_exports || bundleRun.artifactExports;
  if (Array.isArray(artifacts)) {
    return artifacts;
  }
  if (plainObject(artifacts)) {
    return artifacts;
  }
  return undefined;
}

function normalizeDatamachineAgentBundleResult(bundleRun, config = {}, options = {}) {
  const bundle = plainObject(bundleRun.bundle) ? bundleRun.bundle : {};
  const workflowSteps = Array.isArray(bundleRun.workflow?.steps) ? bundleRun.workflow.steps : [];
  const legacyProjectionOutputs = typeof options.legacyProjectionOutputs === 'function'
    ? options.legacyProjectionOutputs(bundleRun.engine_data, config)
    : {};
  const mergeTypedArtifactOutputs = typeof options.mergeTypedArtifactOutputs === 'function'
    ? options.mergeTypedArtifactOutputs
    : (outputs) => outputs;
  const outputs = mergeTypedArtifactOutputs({
    ...(plainObject(bundleRun.outputs) ? bundleRun.outputs : {}),
    ...(plainObject(legacyProjectionOutputs) ? legacyProjectionOutputs : {}),
  }, bundleRun.typed_artifacts, bundleRun.typedArtifacts, bundleRun.outputs?.typed_artifacts, bundleRun.outputs?.typedArtifacts);
  const artifacts = normalizeArtifactExports(bundleRun);
  const transcripts = normalizeTranscriptArtifacts(bundleRun);

  return {
    outputs,
    scenarios: [{
      id: config.workload_id || bundle.flow_slug || bundle.bundle_slug || config.agent_slug || config.flow_slug || 'agent-bundle',
      metrics: {
        workflow_step_count: workflowSteps.length,
      },
      metadata: Object.fromEntries(Object.entries({
        schema: bundleRun.schema,
        success: bundleRun.success !== false,
        dry_run: Boolean(bundleRun.dry_run),
        bundle,
        job_id: bundleRun.job_id,
        job_status: bundleRun.job_status,
        wait_result: bundleRun.wait_result,
        artifacts,
        transcripts,
        engine_data: bundleRun.engine_data,
        error: bundleRun.success === false ? bundleRun.error : undefined,
      }).filter(([, value]) => keepMetadataValue(value))),
    }],
  };
}

module.exports = {
  normalizeDatamachineAgentBundleResult,
};
