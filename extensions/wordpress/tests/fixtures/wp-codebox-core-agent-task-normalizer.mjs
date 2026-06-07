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
