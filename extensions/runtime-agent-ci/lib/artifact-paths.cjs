'use strict';

const path = require('node:path');
const {
  ARTIFACT_MANIFEST_FILE,
  ARTIFACT_MANIFEST_SCHEMA,
  ARTIFACT_PATHS_SCHEMA,
  CANONICAL_RUN_ARTIFACT_FILES,
  RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
} = require('./runtime-contracts.cjs');

function runtimeAgentArtifactPaths(options = {}) {
  const provided = options.artifact_paths && typeof options.artifact_paths === 'object' && !Array.isArray(options.artifact_paths) ? options.artifact_paths : {};
  const runDir = firstString(
    provided.run_dir,
    options.run_dir,
    options.env?.HOMEBOY_RUNTIME_AGENT_RUN_DIR,
    process.env.HOMEBOY_RUNTIME_AGENT_RUN_DIR,
  );
  return stripUndefined({
    schema: ARTIFACT_PATHS_SCHEMA,
    run_dir: runDir,
    events: firstString(provided.events, options.events_file, options.env?.HOMEBOY_RUNTIME_AGENT_EVENTS_FILE, process.env.HOMEBOY_RUNTIME_AGENT_EVENTS_FILE, runArtifactPath(runDir, 'events')),
    status: firstString(provided.status, options.status_file, options.env?.HOMEBOY_RUNTIME_AGENT_STATUS_FILE, process.env.HOMEBOY_RUNTIME_AGENT_STATUS_FILE, runArtifactPath(runDir, 'status')),
    results: firstString(provided.results, options.results_file, options.env?.HOMEBOY_RUNTIME_AGENT_RESULTS_FILE, process.env.HOMEBOY_RUNTIME_AGENT_RESULTS_FILE, runArtifactPath(runDir, 'results')),
    outcome: firstString(provided.outcome, options.outcome_file, options.env?.HOMEBOY_AGENT_TASK_OUTCOME_FILE, process.env.HOMEBOY_AGENT_TASK_OUTCOME_FILE, runArtifactPath(runDir, 'outcome')),
    run_outcome_envelope: firstString(provided.run_outcome_envelope, options.run_outcome_envelope_file, options.env?.HOMEBOY_RUNTIME_AGENT_RUN_OUTCOME_ENVELOPE_FILE, process.env.HOMEBOY_RUNTIME_AGENT_RUN_OUTCOME_ENVELOPE_FILE, runArtifactPath(runDir, 'run_outcome_envelope')),
    runner_execution_record: firstString(provided.runner_execution_record, options.runner_execution_record_file, options.env?.HOMEBOY_RUNTIME_AGENT_RUNNER_EXECUTION_RECORD_FILE, process.env.HOMEBOY_RUNTIME_AGENT_RUNNER_EXECUTION_RECORD_FILE, runArtifactPath(runDir, 'runner_execution_record')),
    stderr: firstString(provided.stderr, options.stderr_file, options.env?.HOMEBOY_RUNTIME_AGENT_STDERR_FILE, process.env.HOMEBOY_RUNTIME_AGENT_STDERR_FILE),
    fanout_run: firstString(provided.fanout_run, options.fanout_run_file, options.env?.HOMEBOY_RUNTIME_AGENT_FANOUT_RUN_FILE, process.env.HOMEBOY_RUNTIME_AGENT_FANOUT_RUN_FILE, runArtifactPath(runDir, 'fanout_run')),
    loop_result: firstString(provided.loop_result, options.loop_result_file, options.env?.HOMEBOY_RUNTIME_AGENT_LOOP_RESULT_FILE, process.env.HOMEBOY_RUNTIME_AGENT_LOOP_RESULT_FILE, runArtifactPath(runDir, 'loop_result')),
    loop_policy: firstString(provided.loop_policy, options.loop_policy_file, options.env?.HOMEBOY_RUNTIME_AGENT_LOOP_POLICY_FILE, process.env.HOMEBOY_RUNTIME_AGENT_LOOP_POLICY_FILE, runArtifactPath(runDir, 'loop_policy')),
    artifact_manifest: firstString(provided.artifact_manifest, options.artifact_manifest_file, options.env?.HOMEBOY_RUNTIME_AGENT_ARTIFACT_MANIFEST_FILE, process.env.HOMEBOY_RUNTIME_AGENT_ARTIFACT_MANIFEST_FILE, runDir ? path.join(runDir, ARTIFACT_MANIFEST_FILE) : ''),
  });
}

function artifactManifestRef(artifactPaths = runtimeAgentArtifactPaths()) {
  const manifestPath = artifactPaths.artifact_manifest || '';
  return manifestPath ? {
    schema: RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
    manifest_schema: ARTIFACT_MANIFEST_SCHEMA,
    path: manifestPath,
  } : null;
}

function artifactManifestForFiles(artifactPaths = {}, files = []) {
  const root = artifactPaths.run_dir || '';
  return {
    schema: ARTIFACT_MANIFEST_SCHEMA,
    artifacts: normalizeManifestFiles(files)
      .map((file) => manifestEntryForFile(root, file, artifactPaths))
      .filter(Boolean),
  };
}

function normalizeManifestFiles(files) {
  return files
    .map((file) => typeof file === 'string' ? { path: file } : file)
    .filter((file) => file && typeof file === 'object' && typeof file.path === 'string' && file.path.trim() !== '');
}

function manifestEntryForFile(root, file, artifactPaths = {}) {
  const relative = relativeArtifactPath(root, file.path);
  if (!relative) {
    return null;
  }
  const semanticKey = firstString(file.semantic_key, file.semanticKey, canonicalSemanticKeyForPath(artifactPaths, file.path));
  return stripUndefined({
    id: firstString(file.id, file.name, file.role, path.basename(relative)),
    path: relative,
    kind: firstString(file.kind, file.type, 'file'),
    role: firstString(file.role),
    label: firstString(file.label, file.name),
    semantic_key: semanticKey,
    content_type: firstString(file.content_type, file.contentType),
    public_url: firstString(file.public_url, file.publicUrl),
    size_bytes: Number.isFinite(file.size_bytes) ? file.size_bytes : Number.isFinite(file.bytes) ? file.bytes : undefined,
    sha256: firstString(file.sha256),
    metadata: file.metadata && typeof file.metadata === 'object' && !Array.isArray(file.metadata) ? file.metadata : {},
  });
}

function canonicalSemanticKeyForPath(artifactPaths, filePath) {
  const absolute = path.resolve(filePath);
  for (const [key, value] of Object.entries(artifactPaths || {})) {
    if (key === 'schema' || key === 'run_dir' || typeof value !== 'string' || value.trim() === '') {
      continue;
    }
    if (path.resolve(value) === absolute) {
      return key;
    }
  }
  return '';
}

function relativeArtifactPath(root, filePath) {
  const absolute = path.resolve(filePath);
  if (!root) {
    return safeRelativePath(filePath);
  }
  const rootAbsolute = path.resolve(root);
  const relative = path.relative(rootAbsolute, absolute);
  return safeRelativePath(relative);
}

function safeRelativePath(value) {
  const normalized = String(value || '').replaceAll(path.sep, '/');
  if (!normalized || normalized.startsWith('../') || normalized === '..' || normalized.startsWith('/') || normalized.includes('/../') || normalized === '.') {
    return '';
  }
  return normalized;
}

function runArtifactPath(runDir, key) {
  return runDir && CANONICAL_RUN_ARTIFACT_FILES[key] ? path.join(runDir, CANONICAL_RUN_ARTIFACT_FILES[key]) : '';
}

function firstString(...values) {
  for (const value of values) {
    if (typeof value === 'string' && value.trim() !== '') {
      return value.trim();
    }
  }
  return '';
}

function stripUndefined(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined && entry !== ''));
}

module.exports = {
  ARTIFACT_MANIFEST_FILE,
  ARTIFACT_MANIFEST_SCHEMA,
  ARTIFACT_PATHS_SCHEMA,
  CANONICAL_RUN_ARTIFACT_FILES,
  RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
  artifactManifestForFiles,
  artifactManifestRef,
  runtimeAgentArtifactPaths,
};
