'use strict';

const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const ADAPTER_ID = 'homeboy/wp-codebox-apply-adapter/v1';
const CONTENT_DIGEST_PREFIX = 'wp-codebox/artifact-content/v1\nfiles/changed-files.json\n';
const CONTENT_DIGEST_SEPARATOR = '\nfiles/patch.diff\n';
const PROTECTED_BRANCHES = new Set(['main', 'master', 'trunk', 'develop']);

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex');
}

function artifactContentDigest(changedFilesJson, patch) {
  return sha256(`${CONTENT_DIGEST_PREFIX}${changedFilesJson}${CONTENT_DIGEST_SEPARATOR}${patch}`);
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd,
    encoding: 'utf8',
    env: options.env || process.env,
    input: options.input,
  });

  if (result.status !== 0) {
    const detail = [result.stderr, result.stdout].filter(Boolean).join('\n').trim();
    throw new Error(`${command} ${args.join(' ')} failed${detail ? `: ${detail}` : ''}`);
  }

  return (result.stdout || '').trim();
}

function realDirectory(directory, label) {
  if (!directory) {
    throw new Error(`${label} is required`);
  }
  const real = fs.realpathSync(directory);
  if (!fs.statSync(real).isDirectory()) {
    throw new Error(`${label} must be a directory`);
  }
  return real;
}

function loadWpCodeboxArtifactBundle(bundlePath) {
  const directory = realDirectory(bundlePath, 'bundlePath');
  const manifestPath = path.join(directory, 'manifest.json');
  const metadataPath = path.join(directory, 'metadata.json');
  const changedFilesPath = path.join(directory, 'files', 'changed-files.json');
  const patchPath = path.join(directory, 'files', 'patch.diff');
  const reviewPath = path.join(directory, 'files', 'review.json');

  const changedFilesJson = fs.readFileSync(changedFilesPath, 'utf8');
  const patch = fs.readFileSync(patchPath, 'utf8');

  return {
    id: readJson(manifestPath).id,
    directory,
    manifest: readJson(manifestPath),
    metadata: fs.existsSync(metadataPath) ? readJson(metadataPath) : {},
    changed_files: readJson(changedFilesPath),
    review: fs.existsSync(reviewPath) ? readJson(reviewPath) : {},
    changedFilesJson,
    patch,
    paths: {
      manifest: manifestPath,
      metadata: metadataPath,
      changed_files: changedFilesPath,
      patch: patchPath,
      review: reviewPath,
    },
  };
}

function normalizePayload(input) {
  if (input.bundlePath) {
    const bundle = loadWpCodeboxArtifactBundle(input.bundlePath);
    return {
      artifact_id: bundle.id,
      artifact: bundle,
      approved_files: input.approvedFiles || input.approved_files || [],
      patch: bundle.patch,
      patch_sha256: sha256(bundle.patch),
      artifact_content_digest: artifactContentDigest(bundle.changedFilesJson, bundle.patch),
    };
  }

  if (!input.payload || typeof input.payload !== 'object') {
    throw new Error('payload or bundlePath is required');
  }

  return input.payload;
}

function changedFilePaths(changedFiles) {
  return (changedFiles.files || [])
    .map((file) => file && file.path)
    .filter((filePath) => typeof filePath === 'string' && filePath.length > 0);
}

function verifyWpCodeboxPayload(payload) {
  const artifact = payload.artifact || {};
  const changedFiles = artifact.changed_files || {};
  const changedFilesJson = artifact.changedFilesJson || (
    artifact.paths?.changed_files && fs.existsSync(artifact.paths.changed_files)
      ? fs.readFileSync(artifact.paths.changed_files, 'utf8')
      : JSON.stringify(changedFiles, null, 2) + '\n'
  );
  const patch = payload.patch || artifact.patch;
  if (typeof patch !== 'string' || patch.trim() === '') {
    throw new Error('payload.patch must contain the approved canonical patch');
  }

  const contentDigest = artifactContentDigest(changedFilesJson, patch);
  const patchSha256 = sha256(patch);
  const declaredContentDigest = payload.artifact_content_digest || artifact.content_digest || artifact.manifest?.contentDigest?.value;
  const declaredPatchSha256 = payload.patch_sha256 || artifact.review?.evidence?.patchSha256;
  const artifactId = payload.artifact_id || artifact.id;

  if (declaredContentDigest && declaredContentDigest !== contentDigest) {
    throw new Error('artifact content digest mismatch');
  }
  if (declaredPatchSha256 && declaredPatchSha256 !== patchSha256) {
    throw new Error('patch digest mismatch');
  }
  if (artifactId && artifactId !== `artifact-bundle-sha256-${contentDigest}`) {
    throw new Error('artifact id does not match content digest');
  }

  const approvedFiles = Array.isArray(payload.approved_files) ? payload.approved_files : [];
  const changedPaths = changedFilePaths(changedFiles);
  const missingApproval = changedPaths.filter((filePath) => !approvedFiles.includes(filePath));
  if (missingApproval.length > 0) {
    throw new Error(`adapter requires approval for every changed file: ${missingApproval.join(', ')}`);
  }

  return {
    artifactId,
    approvedFiles,
    changedPaths,
    contentDigest,
    patch,
    patchSha256,
  };
}

function currentBranch(worktreePath) {
  return run('git', ['branch', '--show-current'], { cwd: worktreePath });
}

function ensureSafeWorktree(worktreePath, options = {}) {
  const realWorktree = realDirectory(worktreePath, 'worktreePath');
  const topLevel = fs.realpathSync(run('git', ['rev-parse', '--show-toplevel'], { cwd: realWorktree }));
  if (topLevel !== realWorktree) {
    throw new Error(`worktreePath must be the git worktree root: ${topLevel}`);
  }

  const branch = currentBranch(realWorktree);
  if (!options.allowProtectedBranch && PROTECTED_BRANCHES.has(branch)) {
    throw new Error(`refusing to apply artifact on protected branch ${branch}`);
  }

  const status = run('git', ['status', '--porcelain'], { cwd: realWorktree });
  if (status !== '') {
    throw new Error('worktree must be clean before applying an approved artifact');
  }

  return realWorktree;
}

function applyApprovedWpCodeboxArtifact(input) {
  const payload = normalizePayload(input);
  const verified = verifyWpCodeboxPayload(payload);
  const worktreePath = ensureSafeWorktree(input.worktreePath, input);
  const requestedBranch = input.branch || '';

  if (requestedBranch) {
    if (!input.allowProtectedBranch && PROTECTED_BRANCHES.has(requestedBranch)) {
      throw new Error(`refusing to apply artifact on protected branch ${requestedBranch}`);
    }
    run('git', ['checkout', '-B', requestedBranch], { cwd: worktreePath });
  }

  const patchPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'wp-codebox-patch-')), 'patch.diff');
  fs.writeFileSync(patchPath, verified.patch);

  const stripComponents = Number.isInteger(input.patchStrip) ? input.patchStrip : 1;
  run('git', ['apply', '--index', '--binary', `-p${stripComponents}`, patchPath], { cwd: worktreePath });

  const appliedFiles = run('git', ['diff', '--cached', '--name-only'], { cwd: worktreePath })
    .split('\n')
    .filter(Boolean);
  if (appliedFiles.length === 0) {
    throw new Error('approved patch did not stage any files');
  }

  const commitMessage = input.commitMessage || `Apply wp-codebox artifact ${verified.artifactId}`;
  run('git', ['commit', '-m', commitMessage], { cwd: worktreePath });
  const commit = run('git', ['rev-parse', 'HEAD'], { cwd: worktreePath });
  const branch = currentBranch(worktreePath);
  let prUrl = input.prUrl || '';

  if (input.push) {
    run('git', ['push', '-u', input.remote || 'origin', branch], { cwd: worktreePath });
  }

  if (input.openPullRequest) {
    const args = ['pr', 'create', '--fill'];
    if (input.prBase) {
      args.push('--base', input.prBase);
    }
    prUrl = run('gh', args, { cwd: worktreePath });
  }

  return {
    success: true,
    adapter_id: ADAPTER_ID,
    artifact_id: verified.artifactId,
    patch_sha256: verified.patchSha256,
    content_digest: verified.contentDigest,
    approved_files: verified.approvedFiles,
    applied_files: appliedFiles,
    worktree: worktreePath,
    branch,
    commit,
    pr_url: prUrl,
    pr_command: prUrl ? '' : 'gh pr create --fill',
  };
}

module.exports = {
  ADAPTER_ID,
  artifactContentDigest,
  applyApprovedWpCodeboxArtifact,
  loadWpCodeboxArtifactBundle,
  verifyWpCodeboxPayload,
};
