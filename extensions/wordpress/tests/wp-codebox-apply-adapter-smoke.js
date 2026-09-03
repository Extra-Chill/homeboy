'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
  applyApprovedWpCodeboxArtifact,
  normalizeWpCodeboxPreflightAsync,
  verifyWpCodeboxPayload,
  wpCodeboxApplyRequestFromBundleAsync,
  wpCodeboxApplyRequestFromBundle,
} = require('../lib/wp-codebox-apply-adapter');

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: options.cwd, encoding: 'utf8' });
  assert.equal(result.status, 0, `${command} ${args.join(' ')} failed: ${result.stderr || result.stdout}`);
  return (result.stdout || '').trim();
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function createRepo(root, branch) {
  const repo = path.join(root, `repo-${branch.replace(/[^a-z0-9]+/gi, '-')}`);
  fs.mkdirSync(repo, { recursive: true });
  fs.writeFileSync(path.join(repo, 'readme.txt'), 'before\n');
  run('git', ['init', '-b', branch], { cwd: repo });
  run('git', ['config', 'user.email', 'smoke@example.test'], { cwd: repo });
  run('git', ['config', 'user.name', 'Smoke Test'], { cwd: repo });
  run('git', ['add', 'readme.txt'], { cwd: repo });
  run('git', ['commit', '-m', 'Initial fixture'], { cwd: repo });
  return repo;
}

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex');
}

function createBundle(root) {
  const bundle = path.join(root, 'bundle');
  const files = path.join(bundle, 'files');
  fs.mkdirSync(files, { recursive: true });

  const changedFiles = {
    schema: 'wp-codebox/changed-files/v1',
    files: [
      {
        path: '/wordpress/wp-content/plugins/fixture-plugin/readme.txt',
        status: 'modified',
        mountTarget: '/wordpress/wp-content/plugins/fixture-plugin',
        relativePath: 'readme.txt',
      },
    ],
  };
  const changedFilesJson = `${JSON.stringify(changedFiles, null, 2)}\n`;
  const patch = [
    'diff --git a/wordpress/wp-content/plugins/fixture-plugin/readme.txt b/wordpress/wp-content/plugins/fixture-plugin/readme.txt',
    '--- a/wordpress/wp-content/plugins/fixture-plugin/readme.txt',
    '+++ b/wordpress/wp-content/plugins/fixture-plugin/readme.txt',
    '@@ -1,1 +1,1 @@',
    '-before',
    '+after',
    '',
  ].join('\n');
  const contentDigest = 'fixture-content-digest';
  const artifactId = `artifact-bundle-sha256-${contentDigest}`;
  const contentDigestMetadata = {
    algorithm: 'sha256',
    inputs: ['files/changed-files.json', 'files/patch.diff'],
    value: contentDigest,
  };

  writeJson(path.join(bundle, 'manifest.json'), {
    id: artifactId,
    contentDigest: contentDigestMetadata,
    createdAt: '2026-05-19T00:00:00.000Z',
    files: [
      { path: 'files/changed-files.json', kind: 'changed-files' },
      { path: 'files/patch.diff', kind: 'patch' },
      { path: 'files/review.json', kind: 'review' },
    ],
  });
  writeJson(path.join(bundle, 'metadata.json'), {
    id: artifactId,
    contentDigest: contentDigestMetadata,
    artifacts: { changedFiles: 'files/changed-files.json', patch: 'files/patch.diff' },
  });
  writeJson(path.join(files, 'changed-files.json'), changedFiles);
  fs.writeFileSync(path.join(files, 'patch.diff'), patch);
  writeJson(path.join(files, 'review.json'), {
    schema: 'wp-codebox/artifact-review/v1',
    artifactId,
    evidence: {
      patch: 'files/patch.diff',
      patchSha256: sha256(patch),
      artifactContentDigest: contentDigest,
      changedFiles: 'files/changed-files.json',
    },
  });

  return { artifactId, bundle, contentDigest, patch };
}

function createPreflight(fixture, changedFiles) {
  const patchSha256 = sha256(fixture.patch);
  return {
    success: true,
    schema: 'wp-codebox/artifact-apply-preflight/v1',
    artifact_id: fixture.artifactId,
    approved_files: ['/wordpress/wp-content/plugins/fixture-plugin/readme.txt'],
    changed_files: ['/wordpress/wp-content/plugins/fixture-plugin/readme.txt'],
    patch_sha256: patchSha256,
    content_digest: fixture.contentDigest,
    payload: {
      artifact_id: fixture.artifactId,
      artifact: {
        id: fixture.artifactId,
        changed_files: changedFiles,
        review: {
          evidence: {
            patchSha256,
            artifactContentDigest: fixture.contentDigest,
          },
        },
      },
      approved_files: ['/wordpress/wp-content/plugins/fixture-plugin/readme.txt'],
      patch: fixture.patch,
      patch_sha256: patchSha256,
      artifact_content_digest: fixture.contentDigest,
      artifact_verification: { valid: true },
    },
  };
}

async function main() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'wp-codebox-apply-adapter-'));

  try {
  const fixture = createBundle(root);
  const changedFilesJson = fs.readFileSync(path.join(fixture.bundle, 'files', 'changed-files.json'), 'utf8');
  const changedFiles = JSON.parse(changedFilesJson);
  const preflight = createPreflight(fixture, changedFiles);
  const repo = createRepo(root, 'feature/apply-smoke');

  const result = applyApprovedWpCodeboxArtifact({
    preflight,
    worktreePath: repo,
    patchStrip: 5,
  });

  assert.equal(result.success, true);
  assert.equal(result.schema, 'homeboy/apply-result/v1');
  assert.equal(result.status, 'applied');
  assert.equal(result.applied, true);
  assert.deepEqual(result.files_changed, ['readme.txt']);
  assert.equal(result.metadata.adapter_id, 'homeboy/wp-codebox-apply-adapter/v1');
  assert.equal(result.metadata.apply_phase.staged, true);
  assert.equal(result.artifacts[0].type, 'wp_codebox_patch');
  assert.equal(result.artifacts[0].approval_scope.scope, 'artifact');
  assert.equal(result.artifact_id, fixture.artifactId);
  assert.equal(result.patch_sha256, sha256(fixture.patch));
  assert.equal(result.content_digest, fixture.contentDigest);
  assert.deepEqual(result.applied_files, ['readme.txt']);
  assert.equal(fs.readFileSync(path.join(repo, 'readme.txt'), 'utf8'), 'after\n');
  assert.equal(run('git', ['status', '--porcelain'], { cwd: repo }), 'M  readme.txt');

  assert.equal(
    verifyWpCodeboxPayload(preflight.payload).contentDigest,
    fixture.contentDigest
  );

  assert.throws(
    () => verifyWpCodeboxPayload({
      artifact_id: fixture.artifactId,
      artifact: {
        changed_files: changedFiles,
      },
      approved_files: [],
      patch: fixture.patch,
      patch_sha256: sha256(fixture.patch),
    }),
    /artifact_content_digest is required/
  );

  const preflightPath = path.join(root, 'preflight.json');
  writeJson(preflightPath, preflight);

  const cliRepo = createRepo(root, 'feature/apply-cli-smoke');
  const cliOutput = run(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'wp-codebox-apply-adapter.cjs'),
    '--preflight',
    preflightPath,
    '--worktree',
    cliRepo,
    '--approved-file',
    '/wordpress/wp-content/plugins/fixture-plugin/readme.txt',
    '--patch-strip',
    '5',
  ]);
  const cliResult = JSON.parse(cliOutput);
  assert.equal(cliResult.success, true);
  assert.equal(cliResult.artifact_id, fixture.artifactId);
  assert.equal(cliResult.branch, 'feature/apply-cli-smoke');
  assert.deepEqual(cliResult.applied_files, ['readme.txt']);
  assert.equal(fs.readFileSync(path.join(cliRepo, 'readme.txt'), 'utf8'), 'after\n');

  const requestRepo = createRepo(root, 'feature/apply-request-smoke');
  const request = wpCodeboxApplyRequestFromBundle({
    preflight,
    worktreePath: requestRepo,
    patchStrip: 5,
  });
  const requestResult = applyApprovedWpCodeboxArtifact({ applyRequest: request });
  assert.equal(requestResult.request_id, request.id);
  assert.equal(requestResult.status, 'applied');
  assert.deepEqual(requestResult.files_changed, ['readme.txt']);
  assert.equal(fs.readFileSync(path.join(requestRepo, 'readme.txt'), 'utf8'), 'after\n');

  const requestPath = path.join(root, 'apply-request.json');
  writeJson(requestPath, wpCodeboxApplyRequestFromBundle({
    preflight,
    patchStrip: 5,
  }));
  const requestCliRepo = createRepo(root, 'feature/apply-request-cli-smoke');
  const requestCliOutput = run(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'wp-codebox-apply-adapter.cjs'),
    '--request',
    requestPath,
    '--worktree',
    requestCliRepo,
  ]);
  const requestCliResult = JSON.parse(requestCliOutput);
  assert.equal(requestCliResult.status, 'applied');
  assert.equal(requestCliResult.request_id, `apply-request-${fixture.artifactId}`);
  assert.deepEqual(requestCliResult.files_changed, ['readme.txt']);
  assert.equal(fs.readFileSync(path.join(requestCliRepo, 'readme.txt'), 'utf8'), 'after\n');

  const corePreflight = await normalizeWpCodeboxPreflightAsync({
    preflight,
  });
  assert.equal(corePreflight.ready, true);
  assert.equal(corePreflight.payload.artifact_id, fixture.artifactId);
  await assert.rejects(
    normalizeWpCodeboxPreflightAsync({
      payload: { force_not_ready: true },
    }),
    /payload.patch must contain the approved canonical patch/
  );
  const coreRequest = await wpCodeboxApplyRequestFromBundleAsync({
    preflight,
  });
  assert.equal(coreRequest.id, `apply-request-${fixture.artifactId}`);
  assert.deepEqual(coreRequest.policy.approved_files, ['/wordpress/wp-content/plugins/fixture-plugin/readme.txt']);

  const fakeWpCli = path.join(root, 'fake-wp-cli.cjs');
  const fakeWpCliCapture = path.join(root, 'fake-wp-cli-capture.json');
  fs.writeFileSync(fakeWpCli, [
    '#!/usr/bin/env node',
    "const fs = require('node:fs');",
    `fs.writeFileSync(${JSON.stringify(fakeWpCliCapture)}, JSON.stringify(process.argv.slice(2), null, 2));`,
    `process.stdout.write(${JSON.stringify(JSON.stringify(preflight))});`,
    '',
  ].join('\n'));
  fs.chmodSync(fakeWpCli, 0o755);
  const delegatedRepo = createRepo(root, 'feature/apply-delegated-preflight-smoke');
  const delegatedResult = applyApprovedWpCodeboxArtifact({
    bundlePath: fixture.bundle,
    worktreePath: delegatedRepo,
    approvedFiles: ['/wordpress/wp-content/plugins/fixture-plugin/readme.txt'],
    wpCli: fakeWpCli,
    patchStrip: 5,
  });
  assert.equal(delegatedResult.status, 'applied');
  const delegatedArgs = JSON.parse(fs.readFileSync(fakeWpCliCapture, 'utf8'));
  assert.deepEqual(delegatedArgs.slice(0, 5), ['wp-codebox', 'artifacts', 'apply-preflight', '--bundle', fs.realpathSync(fixture.bundle)]);
  assert.equal(delegatedArgs.includes('--approved-file'), true);

  console.log('WP Codebox apply adapter smoke passed');
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
