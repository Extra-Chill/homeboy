'use strict';

const TYPED_ARTIFACT_SCHEMA = 'homeboy/agent-task-typed-artifact/v1';

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

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function cleanObject(value) {
  return Object.fromEntries(Object.entries(value)
    .filter(([, entry]) => entry !== undefined && entry !== '' && !(Array.isArray(entry) && entry.length === 0)));
}

module.exports = {
  TYPED_ARTIFACT_SCHEMA,
  artifactNameFromDeclaration,
  artifactPath,
  normalizeTypedArtifactEntry,
  normalizeTypedArtifacts,
  typedArtifactFileRefs,
};
