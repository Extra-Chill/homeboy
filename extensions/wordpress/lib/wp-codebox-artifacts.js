'use strict';

/**
 * External dependencies
 */
const path = require('node:path');

function wpCodeboxArtifactDirectory(codeboxResult, fallbackDirectory) {
  return codeboxResult?.artifacts?.directory
    || codeboxResult?.artifactsDir
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

function normalizeWpCodeboxArtifactPathname(value) {
  return String(value || '').replace(/\\/g, '/').replace(/^\.\//, '');
}

function wpCodeboxArtifactManifestFiles(source) {
  const candidates = [
    source?.artifacts?.files,
    source?.artifacts?.manifest?.files,
    source?.manifest?.artifacts?.files,
    source?.manifest?.files,
  ];

  for (const candidate of candidates) {
    if (Array.isArray(candidate)) {
      return candidate;
    }
    if (candidate && typeof candidate === 'object') {
      return Object.entries(candidate).map(([name, value]) => (
        typeof value === 'string'
          ? { name, path: value }
          : { name, ...value }
      ));
    }
  }

  return [];
}

function wpCodeboxArtifactManifestEntryPath(entry) {
  if (!entry || typeof entry !== 'object') {
    return '';
  }

  return entry.path || entry.pathname || entry.file || entry.relativePath || entry.relative_path || '';
}

function wpCodeboxArtifactManifestEntryMatches(entry, relativePath) {
  const wanted = normalizeWpCodeboxArtifactPathname(relativePath);
  const values = [
    entry?.path,
    entry?.pathname,
    entry?.file,
    entry?.relativePath,
    entry?.relative_path,
    entry?.name,
    entry?.id,
    entry?.label,
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
  resolveWpCodeboxManifestArtifactPath,
  wpCodeboxArtifactManifestV1,
  wpCodeboxArtifactByKey,
  wpCodeboxArtifactDirectory,
  wpCodeboxArtifactManifestFiles,
  wpCodeboxArtifactPath,
  wpCodeboxBrowserArtifacts,
};
