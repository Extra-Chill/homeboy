#!/usr/bin/env node
'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');

/**
 * Internal dependencies
 */
const { applyApprovedWpCodeboxArtifact } = require('../../lib/wp-codebox-apply-adapter');

function argValue(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function hasArg(name) {
  return process.argv.includes(name);
}

function usage() {
  console.error('Usage: wp-codebox-apply-adapter.cjs (--bundle <artifact-dir> --approved-file <sandbox-path> | --request <apply-request.json>) --worktree <path> [--branch <branch>] [--patch-strip <n>] [--push] [--open-pr]');
  process.exit(1);
}

const requestPath = argValue('--request');
const bundlePath = argValue('--bundle');
const worktreePath = argValue('--worktree');
const branch = argValue('--branch');
const commitMessage = argValue('--commit-message');
const patchStripValue = argValue('--patch-strip');
const approvedFiles = [];

for (let index = 0; index < process.argv.length; index += 1) {
  if (process.argv[index] === '--approved-file' && process.argv[index + 1]) {
    approvedFiles.push(process.argv[index + 1]);
  }
}

if (!worktreePath || (!requestPath && (!bundlePath || approvedFiles.length === 0))) {
  usage();
}

try {
  const applyRequest = requestPath ? JSON.parse(fs.readFileSync(requestPath, 'utf8')) : undefined;
  const result = applyApprovedWpCodeboxArtifact({
    applyRequest,
    bundlePath,
    worktreePath,
    branch,
    commitMessage,
    approvedFiles,
    patchStrip: patchStripValue ? Number(patchStripValue) : undefined,
    push: hasArg('--push'),
    openPullRequest: hasArg('--open-pr'),
    prBase: argValue('--base') || undefined,
  });
  console.log(JSON.stringify(result, null, 2));
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
