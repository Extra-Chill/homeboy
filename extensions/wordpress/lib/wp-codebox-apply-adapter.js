'use strict';

/**
 * External dependencies
 */
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const ADAPTER_ID = 'homeboy/wp-codebox-apply-adapter/v1';
const APPLY_RESULT_SCHEMA = 'homeboy/apply-result/v1';
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

function wpCodeboxChangeArtifactFromBundle(bundle, options = {}) {
  const contentDigest = artifactContentDigest(bundle.changedFilesJson, bundle.patch);
  const patchSha256 = sha256(bundle.patch);
  const artifactId = bundle.id || `artifact-bundle-sha256-${contentDigest}`;

  return {
    id: artifactId,
    type: 'wp_codebox_patch',
    provenance: {
      source: 'wp-codebox',
      ...(options.runId ? { run_id: options.runId } : {}),
      ...(options.stepId ? { step_id: options.stepId } : {}),
      ...(options.command ? { command: options.command } : {}),
      ...(bundle.manifest?.createdAt ? { captured_at: bundle.manifest.createdAt } : {}),
    },
    title: options.title || `WP Codebox artifact ${artifactId}`,
    summary: options.summary || 'Approved WP Codebox patch artifact.',
    path: bundle.directory,
    files: changedFilePaths(bundle.changed_files),
    approval_scope: {
      scope: 'artifact',
      artifact_id: artifactId,
    },
    metadata: {
      wp_codebox: {
        bundle_path: bundle.directory,
        content_digest: contentDigest,
        patch_sha256: patchSha256,
        review: bundle.review,
        changed_files: bundle.changed_files,
      },
    },
  };
}

function wpCodeboxApplyRequestFromBundle(options) {
  const bundle = options.bundle || loadWpCodeboxArtifactBundle(options.bundlePath);
  const artifact = wpCodeboxChangeArtifactFromBundle(bundle, options);
  const wpCodeboxMetadata = artifact.metadata.wp_codebox;
  const approvedFiles = options.approvedFiles || options.approved_files || [];

  return {
    id: options.id || `apply-request-${artifact.id}`,
    artifact,
    approval_scope: artifact.approval_scope,
    inputs: {
      bundlePath: bundle.directory,
      ...(options.worktreePath ? { worktreePath: options.worktreePath } : {}),
      ...(options.branch ? { branch: options.branch } : {}),
      ...(options.commitMessage ? { commitMessage: options.commitMessage } : {}),
      ...(Number.isInteger(options.patchStrip) ? { patchStrip: options.patchStrip } : {}),
    },
    policy: {
      approved_files: approvedFiles,
      content_digest: wpCodeboxMetadata.content_digest,
      patch_sha256: wpCodeboxMetadata.patch_sha256,
      publish: {
        push: Boolean(options.push),
        open_pull_request: Boolean(options.openPullRequest),
        ...(options.prBase ? { base: options.prBase } : {}),
        ...(options.remote ? { remote: options.remote } : {}),
      },
    },
  };
}

function normalizeApplyRequest(input) {
  if (input.applyRequest && typeof input.applyRequest === 'object') {
    return input.applyRequest;
  }
  if (input.artifact && input.id) {
    return input;
  }
  return null;
}

function normalizePayload(input) {
  const applyRequest = normalizeApplyRequest(input);
  if (applyRequest) {
    const artifact = applyRequest.artifact || {};
    const controls = {
      inputs: applyRequest.inputs || {},
      policy: applyRequest.policy || {},
    };
    const wpCodeboxMetadata = artifact.metadata?.wp_codebox || {};
    const bundlePath = controls.inputs.bundlePath || controls.inputs.bundle_path || wpCodeboxMetadata.bundle_path || artifact.path;
    const bundle = bundlePath ? loadWpCodeboxArtifactBundle(bundlePath) : null;
    const changedFilesJson = bundle?.changedFilesJson || JSON.stringify(wpCodeboxMetadata.changed_files || {}, null, 2) + '\n';
    const patch = artifact.diff || bundle?.patch;

    return {
      artifact_id: artifact.id,
      artifact: bundle || {
        id: artifact.id,
        changed_files: wpCodeboxMetadata.changed_files || {},
        changedFilesJson,
        review: wpCodeboxMetadata.review || {},
      },
      approved_files: controls.policy.approved_files || controls.inputs.approvedFiles || controls.inputs.approved_files || [],
      patch,
      patch_sha256: controls.policy.patch_sha256 || wpCodeboxMetadata.patch_sha256,
      artifact_content_digest: controls.policy.content_digest || wpCodeboxMetadata.content_digest,
      applyRequest,
      applyInputs: controls.inputs,
      applyPolicy: controls.policy,
    };
  }

  if (input.bundlePath) {
    const bundle = loadWpCodeboxArtifactBundle(input.bundlePath);
    return {
      artifact_id: bundle.id,
      artifact: bundle,
      approved_files: input.approvedFiles || input.approved_files || [],
      patch: bundle.patch,
      patch_sha256: sha256(bundle.patch),
      artifact_content_digest: artifactContentDigest(bundle.changedFilesJson, bundle.patch),
      applyRequest: wpCodeboxApplyRequestFromBundle(input),
      applyInputs: input,
      applyPolicy: {},
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
  const applyInputs = payload.applyInputs || {};
  const applyPolicy = payload.applyPolicy || {};
  const worktreePath = ensureSafeWorktree(input.worktreePath || applyInputs.worktreePath, input);
  const requestedBranch = input.branch || applyInputs.branch || '';

  if (requestedBranch) {
    if (!input.allowProtectedBranch && PROTECTED_BRANCHES.has(requestedBranch)) {
      throw new Error(`refusing to apply artifact on protected branch ${requestedBranch}`);
    }
    run('git', ['checkout', '-B', requestedBranch], { cwd: worktreePath });
  }

  const patchPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'wp-codebox-patch-')), 'patch.diff');
  fs.writeFileSync(patchPath, verified.patch);

  const patchStrip = input.patchStrip ?? applyInputs.patchStrip ?? applyPolicy.patch_strip;
  const stripComponents = Number.isInteger(patchStrip) ? patchStrip : 1;
  run('git', ['apply', '--index', '--binary', `-p${stripComponents}`, patchPath], { cwd: worktreePath });

  const appliedFiles = run('git', ['diff', '--cached', '--name-only'], { cwd: worktreePath })
    .split('\n')
    .filter(Boolean);
  if (appliedFiles.length === 0) {
    throw new Error('approved patch did not stage any files');
  }

  const commitMessage = input.commitMessage || applyInputs.commitMessage || `Apply wp-codebox artifact ${verified.artifactId}`;
  run('git', ['commit', '-m', commitMessage], { cwd: worktreePath });
  const commit = run('git', ['rev-parse', 'HEAD'], { cwd: worktreePath });
  const branch = currentBranch(worktreePath);
  let prUrl = input.prUrl || '';
  const publishPolicy = applyPolicy.publish || {};
  const shouldPush = input.push ?? publishPolicy.push;
  const shouldOpenPullRequest = input.openPullRequest ?? publishPolicy.open_pull_request;

  if (shouldPush) {
    run('git', ['push', '-u', input.remote || publishPolicy.remote || 'origin', branch], { cwd: worktreePath });
  }

  if (shouldOpenPullRequest) {
    const args = ['pr', 'create', '--fill'];
    if (input.prBase || publishPolicy.base) {
      args.push('--base', input.prBase || publishPolicy.base);
    }
    prUrl = run('gh', args, { cwd: worktreePath });
  }

  const resultArtifact = payload.applyRequest?.artifact || wpCodeboxChangeArtifactFromBundle(payload.artifact, {
    title: `Applied WP Codebox artifact ${verified.artifactId}`,
  });

  const legacy = {
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

  return {
    schema: APPLY_RESULT_SCHEMA,
    id: `apply-${verified.artifactId}`,
    request_id: payload.applyRequest?.id,
    status: 'applied',
    applied: true,
    files_changed: appliedFiles,
    artifacts: [resultArtifact],
    warnings: [],
    error: null,
    metadata: {
      adapter_id: ADAPTER_ID,
      wp_codebox: {
        artifact_id: verified.artifactId,
        patch_sha256: verified.patchSha256,
        content_digest: verified.contentDigest,
        approved_files: verified.approvedFiles,
      },
      apply_phase: {
        committed: true,
        commit,
        branch,
        worktree: worktreePath,
      },
      publish_phase: {
        compatibility_behavior: true,
        pushed: Boolean(shouldPush),
        pull_request_opened: Boolean(prUrl),
        pr_url: prUrl,
        pr_command: prUrl ? '' : 'gh pr create --fill',
      },
      legacy,
    },
    ...legacy,
  };
}

module.exports = {
  ADAPTER_ID,
  APPLY_RESULT_SCHEMA,
  artifactContentDigest,
  applyApprovedWpCodeboxArtifact,
  loadWpCodeboxArtifactBundle,
  verifyWpCodeboxPayload,
  wpCodeboxApplyRequestFromBundle,
  wpCodeboxChangeArtifactFromBundle,
};
