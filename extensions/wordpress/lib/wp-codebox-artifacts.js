'use strict';

/**
 * External dependencies
 */
const path = require('node:path');

function wpCodeboxArtifactDirectory(codeboxResult, fallbackDirectory) {
  return codeboxResult?.artifacts?.directory
    || codeboxResult?.artifacts?.path
    || codeboxResult?.artifactsDirectory
    || codeboxResult?.artifacts_directory
    || codeboxResult?.artifactDirectory
    || codeboxResult?.artifact_directory
    || fallbackDirectory
    || '';
}

function wpCodeboxArtifactPath(artifact) {
  if (typeof artifact === 'string') {
    return artifact;
  }
  if (!artifact || typeof artifact !== 'object') {
    return '';
  }
  return artifact.path || artifact.file || artifact.href || '';
}

function wpCodeboxArtifactByKey(source, key) {
  if (!source || !key) {
    return null;
  }
  return source?.artifacts?.[key]
    || source?.files?.[key]
    || source?.artifactFiles?.[key]
    || source?.artifact_files?.[key]
    || source?.artifact?.files?.[key]
    || source?.summary?.files?.[key]
    || source?.artifact?.summary?.files?.[key]
    || null;
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
  resolveWpCodeboxArtifactPath,
  wpCodeboxArtifactByKey,
  wpCodeboxArtifactDirectory,
  wpCodeboxArtifactPath,
};
