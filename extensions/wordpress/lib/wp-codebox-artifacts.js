'use strict';

/**
 * External dependencies
 */
const path = require('node:path');

const WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA = 'wp-codebox/artifact-declaration/v1';
const AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA = 'homeboy/agent-task-artifact-declaration/v1';

function wpCodeboxArtifactDeclarationFromHomeboy(declaration) {
  if (typeof declaration === 'string') {
    return {
      schema: WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
      name: declaration,
      required: true,
    };
  }
  if (!declaration || typeof declaration !== 'object' || Array.isArray(declaration)) {
    return null;
  }
  const name = declaration.name || declaration.id || declaration.output || declaration.artifact;
  if (!name || typeof name !== 'string') {
    return null;
  }
  const artifactSchema = declaration.artifact_schema
    || declaration.artifactSchema
    || declaration.content_schema
    || declaration.contentSchema
    || (declaration.schema && ![
      AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA,
      WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
    ].includes(declaration.schema) ? declaration.schema : undefined);
  return Object.fromEntries(Object.entries({
    schema: WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
    name,
    type: declaration.type || declaration.kind || declaration.artifact_type || declaration.artifactType,
    artifact_schema: artifactSchema,
    path: declaration.path,
    required: declaration.required === undefined ? true : declaration.required === true,
    description: declaration.description,
    metadata: declaration.metadata,
  }).filter(([, value]) => value !== undefined));
}

function normalizeWpCodeboxArtifactPathname(value) {
  return String(value || '').replace(/\\/g, '/').replace(/^\.\//, '');
}

function wpCodeboxArtifactDirectory(codeboxResult, fallbackDirectory) {
  return codeboxResult?.artifacts?.directory || fallbackDirectory || '';
}

function wpCodeboxArtifactPath(artifact) {
  if (typeof artifact === 'string') {
    return artifact;
  }
  if (!artifact || typeof artifact !== 'object') {
    return '';
  }
  return artifact.path || '';
}

function wpCodeboxArtifactManifestFiles(source) {
  return Array.isArray(source?.artifacts?.files) ? source.artifacts.files : [];
}

function wpCodeboxArtifactManifestEntryPath(entry) {
  if (!entry || typeof entry !== 'object') {
    return '';
  }

  return entry.path || '';
}

function wpCodeboxArtifactManifestEntryMatches(entry, relativePath) {
  const wanted = normalizeWpCodeboxArtifactPathname(relativePath);
  const values = [
    entry?.relative_path,
    entry?.relativePath,
    entry?.path,
  ].map(normalizeWpCodeboxArtifactPathname).filter(Boolean);

  return values.some((value) => value === wanted || value.endsWith(`/${wanted}`));
}

function wpCodeboxArtifactManifestV1(source = {}) {
  return {
    version: 1,
    directory: wpCodeboxArtifactDirectory(source, ''),
    files: wpCodeboxArtifactManifestFiles(source),
  };
}

function resolveWpCodeboxManifestArtifactPath(source, relativePath) {
  const artifactDirectory = wpCodeboxArtifactDirectory(source, '');
  if (!artifactDirectory) {
    return '';
  }

  const entry = wpCodeboxArtifactManifestFiles(source).find((candidate) => (
    wpCodeboxArtifactManifestEntryMatches(candidate, relativePath)
  ));
  const entryPath = wpCodeboxArtifactManifestEntryPath(entry);
  if (entryPath) {
    return path.isAbsolute(entryPath) ? entryPath : path.join(artifactDirectory, entryPath);
  }

  return path.join(artifactDirectory, relativePath);
}

function wpCodeboxBrowserArtifacts(source, names = []) {
  const result = {
    directory: resolveWpCodeboxManifestArtifactPath(source, 'files/browser'),
  };

  for (const name of names) {
    result[name] = resolveWpCodeboxManifestArtifactPath(source, `files/browser/${name}`);
  }

  return result;
}

function wpCodeboxArtifactByKey(source, key) {
  if (!source || !key || !Array.isArray(source?.artifacts?.files)) {
    return null;
  }

  return source.artifacts.files.find((entry) => entry?.name === key || entry?.id === key) || null;
}

function resolveWpCodeboxArtifactPath({
  codeboxResult,
  artifactsDirectory,
  source,
  key,
  artifact,
  fallbackPath,
} = {}) {
  const artifactReference = artifact
    || wpCodeboxArtifactByKey(source || codeboxResult, key)
    || fallbackPath;
  const artifactPath = wpCodeboxArtifactPath(artifactReference);
  if (!artifactPath) {
    throw new Error(`Unable to resolve WP Codebox artifact${key ? `: ${key}` : ''}.`);
  }
  if (path.isAbsolute(artifactPath)) {
    return artifactPath;
  }
  const artifactDirectory = wpCodeboxArtifactDirectory(codeboxResult, artifactsDirectory);
  if (!artifactDirectory) {
    throw new Error(`Unable to resolve relative WP Codebox artifact without artifact directory: ${artifactPath}`);
  }
  return path.join(artifactDirectory, artifactPath);
}

module.exports = {
  WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
  resolveWpCodeboxArtifactPath,
  resolveWpCodeboxManifestArtifactPath,
  wpCodeboxArtifactByKey,
  wpCodeboxArtifactDeclarationFromHomeboy,
  wpCodeboxArtifactDirectory,
  wpCodeboxArtifactManifestFiles,
  wpCodeboxArtifactManifestV1,
  wpCodeboxArtifactPath,
  wpCodeboxBrowserArtifacts,
};
