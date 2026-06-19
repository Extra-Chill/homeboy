'use strict';

const TYPED_ARTIFACT_SCHEMA = 'homeboy/agent-task-typed-artifact/v1';
const WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA = 'wp-codebox/artifact-declaration/v1';

const ARTIFACT_ROLE_FALLBACK_PATTERNS = [
  ['patch', /patch|diff/i],
  ['changed_files', /changed[-_ ]?files/i],
  ['transcript', /transcript|conversation|messages/i],
  ['typed_artifact', /typed[-_ ]?bundle[-_ ]?output|typed[-_ ]?artifact/i],
  ['replay_bundle', /replay[-_ ]?bundle/i],
  ['pull_request', /pull[-_ ]?request/i],
  ['screenshot', /screenshot/i],
  ['probe_result', /probe/i],
  ['side_effects', /side[-_ ]?effects?/i],
  ['preflight_evidence', /command[-_ ]?evidence|agent[-_ ]?task[-_ ]?input|homeboy-codebox-task-runner\.json/i],
  ['command_log', /command[-_ ]?log/i],
  ['runtime_log', /runtime[-_ ]?log|startup[-_ ]?log/i],
  ['artifact_bundle', /artifact[-_ ]?bundle|artifact[-_ ]?directory|session[-_ ]?artifacts/i],
];

function normalizeTypedArtifactEntry(name, artifact, options = {}) {
  if (!plainObject(artifact)) {
    return null;
  }
  const artifactName = artifact.name || name;
  if (!artifactName) {
    return null;
  }
  const entry = cleanObject({
    schema: TYPED_ARTIFACT_SCHEMA,
    name: artifactName,
    type: artifact.type || artifact.kind || artifact.artifact_type || artifact.artifactType,
    artifact_schema: artifact.artifact_schema || artifact.artifactSchema || artifact.schema,
    payload: artifact.payload !== undefined ? artifact.payload : artifact.data,
    provenance: plainObject(artifact.provenance) ? artifact.provenance : {},
    file_refs: typedArtifactFileRefs(artifact),
    metadata: plainObject(artifact.metadata) ? artifact.metadata : {},
  });
  return typeof options.sanitize === 'function' ? options.sanitize(entry) : entry;
}

function normalizeTypedArtifacts(value, options = {}) {
  if (Array.isArray(value)) {
    return Object.fromEntries(value
      .map((artifact, index) => normalizeTypedArtifactEntry(artifact?.name || artifact?.id || `artifact_${index + 1}`, artifact, options))
      .filter(Boolean)
      .map((artifact) => [artifact.name, artifact]));
  }
  if (!plainObject(value)) {
    return {};
  }
  return Object.fromEntries(Object.entries(value)
    .map(([name, artifact]) => normalizeTypedArtifactEntry(name, artifact, options))
    .filter(Boolean)
    .map((artifact) => [artifact.name, artifact]));
}

function typedArtifactsFromCodeboxResult(result, options = {}) {
  const workload = options.workload || agentRuntimeWorkload(result) || {};
  const scenarios = Array.isArray(workload.scenarios) ? workload.scenarios : [];
  const candidates = [
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
  return Object.assign({}, ...candidates.map((candidate) => normalizeTypedArtifacts(candidate, options)));
}

function normalizeCodeboxArtifactDeclaration(defaultName, declaration, options = {}) {
  if (typeof declaration === 'string') {
    return {
      schema: WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
      name: declaration,
      required: true,
    };
  }
  if (!plainObject(declaration)) {
    return null;
  }
  const name = declaration.name || declaration.id || declaration.output || declaration.artifact || defaultName;
  if (!name || typeof name !== 'string') {
    return null;
  }
  const ignoredSchemas = new Set([
    WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
    ...normalizeArray(options.ignoredSchemas),
  ]);
  const artifactSchema = declaration.artifact_schema
    || declaration.artifactSchema
    || declaration.content_schema
    || declaration.contentSchema
    || (declaration.schema && !ignoredSchemas.has(declaration.schema) ? declaration.schema : undefined);
  return cleanObject({
    schema: WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
    name,
    type: declaration.type || declaration.kind || declaration.artifact_type || declaration.artifactType,
    artifact_schema: artifactSchema,
    path: declaration.path,
    required: declaration.required === undefined ? true : declaration.required === true,
    description: declaration.description,
    metadata: declaration.metadata,
  });
}

function normalizeCodeboxArtifactOutcome(artifact, rawArtifact = {}, options = {}) {
  if (!artifact) {
    return artifact;
  }
  const nativeKind = rawArtifact.kind || rawArtifact.type || artifact.kind || '';
  const role = artifactRoleFromCodeboxArtifact({ ...artifact, kind: nativeKind }, options.roleAliases || {});
  const metadata = plainObject(artifact.metadata) ? artifact.metadata : {};
  const sanitize = typeof options.sanitize === 'function' ? options.sanitize : (value) => value;
  return {
    ...artifact,
    role,
    metadata: sanitize({
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

function typedArtifactFileRefs(artifact) {
  if (Array.isArray(artifact?.file_refs)) {
    return artifact.file_refs;
  }
  if (Array.isArray(artifact?.fileRefs)) {
    return artifact.fileRefs;
  }
  return [];
}

function artifactNameFromDeclaration(declaration) {
  if (typeof declaration === 'string') {
    return declaration;
  }
  if (!plainObject(declaration)) {
    return '';
  }
  return declaration.name || declaration.id || '';
}

function agentRuntimeWorkload(result = {}) {
  return result.workload || result.metadata?.workload || result.metadata?.agent_runtime?.workload || result.run?.workload || {};
}

function artifactPath(root, relativePath) {
  if (!root || !relativePath) {
    return '';
  }
  return `${String(root).replace(/\/$/, '')}/${String(relativePath).replace(/^\//, '')}`;
}

function artifactRoleFromCodeboxArtifact(artifact = {}, roleAliases = {}) {
  const labels = artifactLabels(artifact);
  const explicitRole = roleFromAliases(labels, roleAliases);
  if (explicitRole) {
    return explicitRole;
  }
  const fallbackLabel = labels.join(' ');
  const fallback = ARTIFACT_ROLE_FALLBACK_PATTERNS.find(([, pattern]) => pattern.test(fallbackLabel));
  return fallback?.[0] || 'artifact';
}

function roleFromAliases(labels, roleAliases = {}) {
  const normalizedLabels = labels.map(normalizeLabel).filter(Boolean);
  const aliasGroups = [
    roleAliases.artifact_roles,
    roleAliases.artifact_kinds,
    roleAliases.artifact_filenames,
  ].filter(plainObject);
  for (const aliases of aliasGroups) {
    for (const [role, values] of Object.entries(aliases)) {
      if (normalizeArray(values).map(normalizeLabel).some((alias) => normalizedLabels.includes(alias))) {
        return role;
      }
    }
  }
  return '';
}

function artifactLabels(artifact = {}) {
  return [
    artifact.role,
    artifact.kind,
    artifact.type,
    artifact.name,
    artifact.id,
    artifact.path,
    artifact.url,
    artifact.file,
    artifact.directory,
    basename(artifact.path || artifact.url || artifact.file || artifact.directory || ''),
  ].filter(Boolean).map(String);
}

function basename(value) {
  return String(value).split(/[\\/]/).filter(Boolean).pop() || '';
}

function normalizeLabel(value) {
  return String(value || '').trim().toLowerCase();
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function cleanObject(value) {
  return Object.fromEntries(Object.entries(value)
    .filter(([, entry]) => entry !== undefined && entry !== '' && !(Array.isArray(entry) && entry.length === 0)));
}

module.exports = {
  TYPED_ARTIFACT_SCHEMA,
  WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
  artifactRoleFromCodeboxArtifact,
  artifactNameFromDeclaration,
  artifactPath,
  normalizeCodeboxArtifactDeclaration,
  normalizeCodeboxArtifactOutcome,
  normalizeTypedArtifactEntry,
  normalizeTypedArtifacts,
  typedArtifactsFromCodeboxResult,
  typedArtifactFileRefs,
};
