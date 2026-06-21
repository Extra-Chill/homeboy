'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  CODEBOX_PROVIDER_ADAPTER_ID,
  loadArtifactBundle,
  normalizeOutcome,
  preflightApply,
  runRecipe,
} = require('../lib/codebox-provider-adapter');

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex');
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function createBundle(root) {
  const bundle = path.join(root, 'bundle');
  const files = path.join(bundle, 'files');
  fs.mkdirSync(files, { recursive: true });

  const changedFiles = {
    schema: 'wp-codebox/changed-files/v1',
    files: [{ path: '/wordpress/wp-content/plugins/fixture/readme.txt', status: 'modified' }],
  };
  const patch = 'diff --git a/readme.txt b/readme.txt\n--- a/readme.txt\n+++ b/readme.txt\n@@ -1 +1 @@\n-before\n+after\n';
  const contentDigest = sha256('fixture-bundle');
  const patchSha256 = sha256(patch);
  const artifactId = `artifact-bundle-sha256-${contentDigest}`;

  writeJson(path.join(bundle, 'manifest.json'), {
    id: artifactId,
    contentDigest: { algorithm: 'sha256', value: contentDigest },
  });
  writeJson(path.join(bundle, 'metadata.json'), { id: artifactId });
  writeJson(path.join(files, 'changed-files.json'), changedFiles);
  writeJson(path.join(files, 'review.json'), {
    evidence: { patchSha256, artifactContentDigest: contentDigest },
  });
  fs.writeFileSync(path.join(files, 'patch.diff'), patch);

  return { artifactId, bundle, contentDigest, patchSha256 };
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codebox-provider-adapter-'));
const fixture = createBundle(root);
const bundle = loadArtifactBundle(fixture.bundle);
const preflight = preflightApply({ bundle, approvedFiles: ['/wordpress/wp-content/plugins/fixture/readme.txt'] });
const outcome = normalizeOutcome(
  { finding_id: 'finding-1', audit_findings: [{ id: 'finding-1' }], sandbox_session_id: 'sandbox-1' },
  { success: true },
  { id: fixture.artifactId, directory: fixture.bundle },
  true
);

assert.equal(CODEBOX_PROVIDER_ADAPTER_ID, 'homeboy/codebox-provider-adapter/v1');
assert.equal(typeof runRecipe, 'function');
assert.equal(bundle.id, fixture.artifactId);
assert.equal(preflight.schema, 'wp-codebox/artifact-apply-preflight/v1');
assert.equal(preflight.payload.artifact_id, fixture.artifactId);
assert.equal(outcome.kind, 'fix_artifact');
assert.equal(outcome.artifact_id, fixture.artifactId);

console.log('codebox provider adapter smoke passed');
