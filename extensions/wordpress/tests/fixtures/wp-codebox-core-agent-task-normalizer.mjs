export function normalizeAgentTaskRunResult(raw, options = {}) {
  const result = raw && typeof raw === 'object' ? raw : {};
  const runtime = result.run?.runtime || {};
  const agentResult = result.run?.agentResult || result.agentResult || result.agent_result || {};
  const patch = agentResult.patch || {};
  const changedFiles = agentResult.changedFiles || {};
  const status = result.timeout
    ? 'timeout'
    : (result.success === true && result.status === 'completed' && options.exitStatus === 0 ? 'succeeded' : 'failed');

  return {
    schema: 'wp-codebox/agent-task-run-result/v1',
    status,
    success: status === 'succeeded',
    summary: result.summary || `fixture normalized ${status}`,
    artifacts: [{
      id: 'fixture-normalized-patch',
      kind: 'codebox-patch',
      path: '/tmp/fixture-normalized/patch.diff',
      sha256: patch.sha256,
    }],
    refs: {
      artifact_bundles: [],
      changed_files: [],
      patches: [],
      transcripts: [],
      logs: [],
      runtimes: [],
    },
    diagnostics: [{ class: 'fixture.normalizer', message: 'Fixture normalizer was used.', data: { exitStatus: options.exitStatus } }],
    metadata: {
      run_id: result.run?.runId,
      run_status: result.run?.status,
      runtime_id: runtime.id,
      runtime_status: runtime.status,
      changed_files_count: changedFiles.count,
      patch_bytes: patch.bytes,
      patch_sha256: patch.sha256,
    },
    no_op: { detected: false },
    failure_classification: status === 'failed' ? 'runtime' : undefined,
  };
}

export function normalizeRecipeRunSummary(raw, options = {}) {
  const result = raw && typeof raw === 'object' ? raw : {};
  const failedPhase = result.failed_phase || (result.probe?.success === false ? 'probe' : undefined);
  const status = failedPhase ? 'failed' : 'succeeded';

  return {
    schema: 'wp-codebox/recipe-run-summary/v1',
    status,
    success: status === 'succeeded',
    failed_phase: failedPhase,
    failure_summary: failedPhase ? `fixture recipe ${failedPhase} failed` : undefined,
    artifacts: [{
      id: 'fixture-normalized-recipe-probe',
      kind: 'recipe-probe-result',
      path: '/tmp/fixture-normalized/recipe-probe.json',
    }],
    refs: {
      startup_logs: [],
      probe_json: [],
      screenshots: [],
      side_effects: [],
      declared_artifacts: [],
      artifact_bundles: [],
      changed_files: [],
      patches: [],
      transcripts: [],
      logs: [],
      runtimes: [],
    },
    diagnostics: [{ class: 'fixture.recipe_normalizer', message: 'Fixture recipe normalizer was used.', data: { exitStatus: options.exitStatus } }],
    metadata: {
      failure_phase: failedPhase,
      recipe_pack: result.pack,
      recipe_name: result.name,
    },
  };
}

export function normalizeRuntimeProfilePayload(payload) {
  return {
    ...payload,
    metadata: {
      ...(payload.metadata || {}),
      fixture_runtime_profile_normalizer: true,
    },
  };
}

export function normalizeTypedArtifactEntry(name, raw) {
  const artifact = raw && typeof raw === 'object' ? raw : {};
  const artifactName = artifact.name || name;
  if (!artifactName) {
    return null;
  }
  return {
    schema: 'homeboy/agent-task-typed-artifact/v1',
    name: artifactName,
    type: artifact.type || artifact.kind,
    artifact_schema: artifact.artifact_schema || artifact.artifactSchema || artifact.schema,
    payload: artifact.payload !== undefined ? artifact.payload : artifact.data,
    provenance: artifact.provenance || {},
    file_refs: artifact.file_refs || artifact.fileRefs || [],
    metadata: {
      ...(artifact.metadata || {}),
      fixture_typed_artifact_normalizer: true,
    },
  };
}
