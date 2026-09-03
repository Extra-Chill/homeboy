'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

/**
 * Internal dependencies
 */
const { createCodeboxClient } = require('./codebox-client');

const ADAPTER_ID = 'homeboy/wp-codebox-apply-adapter/v1';
const APPLY_RESULT_SCHEMA = 'homeboy/apply-result/v1';
const WP_CODEBOX_PREFLIGHT_SCHEMA = 'wp-codebox/artifact-apply-preflight/v1';
const PROTECTED_BRANCHES = new Set(['main', 'master', 'trunk', 'develop']);

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

function artifactIdFromBundlePath(bundlePath) {
  const manifestPath = path.join(realDirectory(bundlePath, 'bundlePath'), 'manifest.json');
  return readJson(manifestPath).id;
}

function loadWpCodeboxArtifactBundle(bundlePath) {
  const directory = realDirectory(bundlePath, 'bundlePath');
  const manifestPath = path.join(directory, 'manifest.json');
  const metadataPath = path.join(directory, 'metadata.json');
  const changedFilesPath = path.join(directory, 'files', 'changed-files.json');
  const patchPath = path.join(directory, 'files', 'patch.diff');
  const reviewPath = path.join(directory, 'files', 'review.json');
  const manifest = readJson(manifestPath);
  const review = fs.existsSync(reviewPath) ? readJson(reviewPath) : {};

  return {
    id: manifest.id,
    directory,
    manifest,
    metadata: fs.existsSync(metadataPath) ? readJson(metadataPath) : {},
    changed_files: readJson(changedFilesPath),
    review,
    patch: fs.readFileSync(patchPath, 'utf8'),
    content_digest: manifest.contentDigest?.value || review.evidence?.artifactContentDigest || '',
    patch_sha256: review.evidence?.patchSha256 || '',
    paths: {
      manifest: manifestPath,
      metadata: metadataPath,
      changed_files: changedFilesPath,
      patch: patchPath,
      review: reviewPath,
    },
  };
}

function preflightPayloadFromBundle(bundle, approvedFiles) {
  return {
    artifact_id: bundle.id,
    artifact: bundle,
    approved_files: approvedFiles,
    patch: bundle.patch,
    patch_sha256: bundle.patch_sha256,
    artifact_content_digest: bundle.content_digest,
    artifact_verification: { valid: true },
  };
}

function wpCodeboxChangeArtifactFromPreflight(payload, options = {}) {
  const artifact = payload.artifact || {};
  const artifactId = payload.artifact_id || artifact.id;

  return {
    id: artifactId,
    type: 'wp_codebox_patch',
    provenance: {
      source: 'wp-codebox',
      ...(options.runId ? { run_id: options.runId } : {}),
      ...(options.stepId ? { step_id: options.stepId } : {}),
      ...(options.command ? { command: options.command } : {}),
      ...(artifact.manifest?.createdAt ? { captured_at: artifact.manifest.createdAt } : {}),
    },
    title: options.title || `WP Codebox artifact ${artifactId}`,
    summary: options.summary || 'Approved WP Codebox patch artifact.',
    path: artifact.directory || artifact.path || '',
    files: payload.changed_files || changedFilePaths(artifact.changed_files || {}),
    approval_scope: {
      scope: 'artifact',
      artifact_id: artifactId,
    },
    metadata: {
      wp_codebox: {
        bundle_path: artifact.directory || artifact.path || '',
        content_digest: payload.artifact_content_digest || payload.content_digest,
        patch_sha256: payload.patch_sha256,
        review: artifact.review || {},
        changed_files: artifact.changed_files || {},
      },
    },
  };
}

function wpCodeboxChangeArtifactFromBundle(bundle, options = {}) {
  return wpCodeboxChangeArtifactFromPreflight(preflightPayloadFromBundle(bundle, changedFilePaths(bundle.changed_files)), options);
}

function wpCodeboxApplyRequestFromBundle(options) {
  const preflight = normalizeWpCodeboxPreflight(options);
  const payload = preflight.payload;
  const artifact = wpCodeboxChangeArtifactFromPreflight(payload, options);
  const approvedFiles = options.approvedFiles || options.approved_files || payload.approved_files || [];

  return {
    id: options.id || `apply-request-${artifact.id}`,
    artifact,
    approval_scope: artifact.approval_scope,
    inputs: {
      ...(options.bundlePath ? { bundlePath: realDirectory(options.bundlePath, 'bundlePath') } : {}),
      ...(options.bundle?.directory ? { bundlePath: realDirectory(options.bundle.directory, 'bundlePath') } : {}),
      ...(options.artifactsPath ? { artifactsPath: realDirectory(options.artifactsPath, 'artifactsPath') } : {}),
      ...(payload.artifact_id ? { artifactId: payload.artifact_id } : {}),
      preflight,
      ...(options.worktreePath ? { worktreePath: options.worktreePath } : {}),
      ...(Number.isInteger(options.patchStrip) ? { patchStrip: options.patchStrip } : {}),
    },
    policy: {
      approved_files: approvedFiles,
      content_digest: payload.artifact_content_digest || payload.content_digest,
      patch_sha256: payload.patch_sha256,
    },
  };
}

function runWpCodeboxApplyPreflight(options) {
  const bundlePath = options.bundlePath ? realDirectory(options.bundlePath, 'bundlePath') : '';
  let artifactsPath = '';
  if (options.artifactsPath) {
    artifactsPath = realDirectory(options.artifactsPath, 'artifactsPath');
  } else if (bundlePath) {
    artifactsPath = path.dirname(bundlePath);
  }
  const artifactId = options.artifactId || options.artifact_id || (bundlePath ? artifactIdFromBundlePath(bundlePath) : '');
  const approvedFiles = options.approvedFiles || options.approved_files || [];

  if (!artifactId || !artifactsPath) {
    throw new Error('artifactId and artifactsPath are required for WP Codebox apply preflight');
  }
  if (!Array.isArray(approvedFiles) || approvedFiles.length === 0) {
    throw new Error('approvedFiles is required for WP Codebox apply preflight');
  }

  return createCodeboxClient(options).runArtifactApplyPreflight({
    artifactId,
    artifactsPath,
    bundlePath,
    approvedFiles,
    cwd: options.cwd,
    env: options.env,
    wpCommand: options.wpCommand,
    wpCli: options.wpCli,
  });
}

function normalizeWpCodeboxPreflight(input) {
  if (input.bundle && typeof input.bundle === 'object') {
    const payload = preflightPayloadFromBundle(input.bundle, input.approvedFiles || input.approved_files || []);
    verifyWpCodeboxPayload(payload);
    return {
      schema: WP_CODEBOX_PREFLIGHT_SCHEMA,
      payload,
    };
  }

  if (input.preflightPath) {
    return normalizeWpCodeboxPreflight(readJson(input.preflightPath));
  }

  const preflight = input.preflight || input.applyPreflight || (
    input.schema === WP_CODEBOX_PREFLIGHT_SCHEMA ? input : null
  );

  if (preflight) {
    if (preflight.schema && preflight.schema !== WP_CODEBOX_PREFLIGHT_SCHEMA) {
      throw new Error(`unsupported WP Codebox preflight schema: ${preflight.schema}`);
    }
    if (preflight.ready === false) {
      const violations = Array.isArray(preflight.violations) ? preflight.violations : [];
      const details = violations.map((violation) => violation.message || violation.code).filter(Boolean).join('; ');
      throw new Error(`WP Codebox apply preflight is not ready${details ? `: ${details}` : ''}`);
    }
    const payload = preflight.payload || preflight;
    verifyWpCodeboxPayload(payload);
    return {
      ...preflight,
      ready: preflight.ready !== false,
      payload,
    };
  }

  if (input.payload && typeof input.payload === 'object') {
    verifyWpCodeboxPayload(input.payload);
    return {
      schema: WP_CODEBOX_PREFLIGHT_SCHEMA,
      payload: input.payload,
    };
  }

  return {
    ...runWpCodeboxApplyPreflight(input),
  };
}

async function normalizeWpCodeboxPreflightAsync(input) {
  return normalizeWpCodeboxPreflight(input);
}

async function wpCodeboxApplyRequestFromBundleAsync(options) {
  return wpCodeboxApplyRequestFromBundle({
    ...options,
    preflight: await normalizeWpCodeboxPreflightAsync(options),
  });
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
    const preflight = normalizeWpCodeboxPreflight({
      ...controls.inputs,
      artifactId: controls.inputs.artifactId || controls.inputs.artifact_id || artifact.id,
      approvedFiles: controls.policy.approved_files || controls.inputs.approvedFiles || controls.inputs.approved_files || [],
    });
    const payload = preflight.payload;

    return {
      ...payload,
      applyRequest,
      applyInputs: controls.inputs,
      applyPolicy: controls.policy,
    };
  }

  const preflight = normalizeWpCodeboxPreflight(input);
  return {
    ...preflight.payload,
    applyInputs: input,
    applyPolicy: {},
  };
}

function changedFilePaths(changedFiles) {
  return (changedFiles.files || [])
    .map((file) => file && file.path)
    .filter((filePath) => typeof filePath === 'string' && filePath.length > 0);
}

function verifyWpCodeboxPayload(payload) {
  const artifact = payload.artifact || {};
  const changedFiles = artifact.changed_files || {};
  const patch = payload.patch || artifact.patch;
  if (typeof patch !== 'string' || patch.trim() === '') {
    throw new Error('WP Codebox preflight payload.patch must contain the approved canonical patch');
  }

  const artifactId = payload.artifact_id || artifact.id;
  if (!artifactId) {
    throw new Error('WP Codebox preflight payload.artifact_id is required');
  }
  const contentDigest = payload.artifact_content_digest || payload.content_digest || artifact.content_digest || artifact.manifest?.contentDigest?.value;
  if (!contentDigest) {
    throw new Error('WP Codebox preflight payload.artifact_content_digest is required');
  }
  const patchSha256 = payload.patch_sha256 || artifact.review?.evidence?.patchSha256;
  if (!patchSha256) {
    throw new Error('WP Codebox preflight payload.patch_sha256 is required');
  }

  const approvedFiles = Array.isArray(payload.approved_files) ? payload.approved_files : [];
  if (approvedFiles.length === 0) {
    throw new Error('WP Codebox preflight payload.approved_files must contain at least one file');
  }
  const changedPaths = changedFilePaths(changedFiles);

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

  const branch = currentBranch(worktreePath);

  const resultArtifact = payload.applyRequest?.artifact || wpCodeboxChangeArtifactFromPreflight(payload, {
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
        staged: true,
        branch,
        worktree: worktreePath,
      },
      legacy,
    },
    ...legacy,
  };
}

module.exports = {
  ADAPTER_ID,
  APPLY_RESULT_SCHEMA,
  applyApprovedWpCodeboxArtifact,
  loadWpCodeboxArtifactBundle,
  normalizeWpCodeboxPreflightAsync,
  normalizeWpCodeboxPreflight,
  runWpCodeboxApplyPreflight,
  verifyWpCodeboxPayload,
  wpCodeboxApplyRequestFromBundleAsync,
  wpCodeboxApplyRequestFromBundle,
  wpCodeboxChangeArtifactFromBundle,
  wpCodeboxChangeArtifactFromPreflight,
};
