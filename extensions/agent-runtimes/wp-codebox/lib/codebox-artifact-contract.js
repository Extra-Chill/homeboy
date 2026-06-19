'use strict';

const TYPED_ARTIFACT_SCHEMA = 'homeboy/agent-task-typed-artifact/v1';

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
  artifactRoleFromCodeboxArtifact,
  artifactNameFromDeclaration,
  artifactPath,
  normalizeTypedArtifactEntry,
  normalizeTypedArtifacts,
  typedArtifactFileRefs,
};
