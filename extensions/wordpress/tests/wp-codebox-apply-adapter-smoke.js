'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
  applyApprovedWpCodeboxArtifact,
  artifactContentDigest,
  verifyWpCodeboxPayload,
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
  const contentDigest = artifactContentDigest(changedFilesJson, patch);
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

  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'wp-codebox-apply-adapter-'));

try {
  const fixture = createBundle(root);
  const changedFilesJson = fs.readFileSync(path.join(fixture.bundle, 'files', 'changed-files.json'), 'utf8');
  const changedFiles = JSON.parse(changedFilesJson);
  const repo = createRepo(root, 'feature/apply-smoke');

  const result = applyApprovedWpCodeboxArtifact({
    bundlePath: fixture.bundle,
    worktreePath: repo,
    branch: 'feature/wp-codebox-apply-smoke',
    commitMessage: 'Apply fixture wp-codebox artifact',
    approvedFiles: ['/wordpress/wp-content/plugins/fixture-plugin/readme.txt'],
    patchStrip: 5,
  });

  assert.equal(result.success, true);
  assert.equal(result.artifact_id, fixture.artifactId);
  assert.equal(result.patch_sha256, sha256(fixture.patch));
  assert.equal(result.content_digest, fixture.contentDigest);
  assert.deepEqual(result.applied_files, ['readme.txt']);
  assert.equal(fs.readFileSync(path.join(repo, 'readme.txt'), 'utf8'), 'after\n');
  assert.equal(run('git', ['status', '--porcelain'], { cwd: repo }), '');

  assert.equal(
    verifyWpCodeboxPayload({
      artifact_id: fixture.artifactId,
      artifact: {
        changed_files: changedFiles,
        paths: { changed_files: path.join(fixture.bundle, 'files', 'changed-files.json') },
      },
      approved_files: ['/wordpress/wp-content/plugins/fixture-plugin/readme.txt'],
      patch: fixture.patch,
      patch_sha256: sha256(fixture.patch),
      artifact_content_digest: fixture.contentDigest,
    }).contentDigest,
    fixture.contentDigest
  );

  assert.throws(
    () => verifyWpCodeboxPayload({
      artifact_id: fixture.artifactId,
      artifact: {
        changed_files: changedFiles,
        changedFilesJson,
      },
      approved_files: [],
      patch: fixture.patch,
      patch_sha256: sha256(fixture.patch),
      artifact_content_digest: fixture.contentDigest,
    }),
    /approval for every changed file/
  );

  const cliRepo = createRepo(root, 'feature/apply-cli-smoke');
  const cliOutput = run(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'wp-codebox-apply-adapter.cjs'),
    '--bundle',
    fixture.bundle,
    '--worktree',
    cliRepo,
    '--branch',
    'feature/wp-codebox-apply-cli-smoke',
    '--approved-file',
    '/wordpress/wp-content/plugins/fixture-plugin/readme.txt',
    '--patch-strip',
    '5',
    '--commit-message',
    'Apply fixture wp-codebox artifact through CLI',
  ]);
  const cliResult = JSON.parse(cliOutput);
  assert.equal(cliResult.success, true);
  assert.equal(cliResult.artifact_id, fixture.artifactId);
  assert.equal(cliResult.branch, 'feature/wp-codebox-apply-cli-smoke');
  assert.deepEqual(cliResult.applied_files, ['readme.txt']);
  assert.equal(fs.readFileSync(path.join(cliRepo, 'readme.txt'), 'utf8'), 'after\n');

  console.log('WP Codebox apply adapter smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
