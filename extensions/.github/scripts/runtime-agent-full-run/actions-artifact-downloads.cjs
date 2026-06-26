#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const { run, parseJsonInput } = require('./lib/common.cjs');

function main() {
  downloadArtifactsFromEnv(process.env);
}

function downloadArtifactsFromEnv(env) {
  const downloads = normalizeArtifactDownloads(env.ACTIONS_ARTIFACT_DOWNLOADS || '[]', env.GITHUB_REPOSITORY || '');
  for (const download of downloads) {
    fs.mkdirSync(download.dir, { recursive: true });
    run('gh', ['run', 'download', download.run_id, '--repo', download.repo, '--name', download.name, '--dir', download.dir]);
  }
  return downloads;
}

function normalizeArtifactDownloads(input, defaultRepo) {
  return parseJsonInput('actions_artifact_downloads', input, 'array', []).map((entry, index) => normalizeArtifactDownload(entry, index, defaultRepo));
}

function normalizeArtifactDownload(entry, index, defaultRepo) {
  if (!entry || Array.isArray(entry) || typeof entry !== 'object') {
    throw new Error(`actions_artifact_downloads[${index}] must be an object`);
  }
  const repo = stringValue(entry.repo) || defaultRepo;
  const runId = stringValue(entry.run_id ?? entry.runId);
  const name = stringValue(entry.name ?? entry.artifact_name ?? entry.artifactName);
  const dir = stringValue(entry.dir ?? entry.destination) || `.ci/actions-artifacts/${name}`;
  if (!repo) {
    throw new Error(`actions_artifact_downloads[${index}].repo is required`);
  }
  if (!runId) {
    throw new Error(`actions_artifact_downloads[${index}].run_id is required`);
  }
  if (!name) {
    throw new Error(`actions_artifact_downloads[${index}].name is required`);
  }
  return { repo, run_id: runId, name, dir };
}

function stringValue(value) {
  return typeof value === 'string' ? value.trim() : '';
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}

module.exports = { downloadArtifactsFromEnv, normalizeArtifactDownloads };
