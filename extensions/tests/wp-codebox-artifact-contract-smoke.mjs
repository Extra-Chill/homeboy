#!/usr/bin/env node
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
  artifactResultEnvelopeFromCodeboxResult,
  artifactRoleFromCodeboxArtifact,
  artifactNameFromDeclaration,
  artifactPath,
  normalizeArtifactResultEnvelope,
  normalizeTypedArtifactEntry,
  normalizeTypedArtifacts,
  typedArtifactsFromCodeboxResult,
  typedArtifactFileRefs,
} = require(path.join(repoRoot, 'agent-runtimes/wp-codebox/lib/codebox-artifact-contract'));

assert.equal(artifactPath('/tmp/artifacts/', '/files/transcript.json'), '/tmp/artifacts/files/transcript.json');
assert.equal(artifactPath('', 'files/transcript.json'), '');
assert.equal(artifactNameFromDeclaration({ id: 'transcript' }), 'transcript');
assert.equal(artifactRoleFromCodeboxArtifact({ kind: 'codebox-patch' }, { artifact_roles: { patch: ['codebox-patch'] } }), 'patch');
assert.equal(artifactRoleFromCodeboxArtifact({ path: '/tmp/files/changed-files.json' }), 'changed_files');

assert.deepEqual(typedArtifactFileRefs({ fileRefs: [{ path: 'artifact.json' }] }), [{ path: 'artifact.json' }]);

assert.deepEqual(normalizeTypedArtifactEntry('packet', {
  kind: 'json',
  schema: 'example/schema/v1',
  data: { ok: true },
  file_refs: [{ path: 'packet.json' }],
  metadata: { visible: true },
}), {
  schema: 'homeboy/agent-task-typed-artifact/v1',
  name: 'packet',
  type: 'json',
  artifact_schema: 'example/schema/v1',
  payload: { ok: true },
  provenance: {},
  file_refs: [{ path: 'packet.json' }],
  metadata: { visible: true },
});

assert.deepEqual(Object.keys(normalizeTypedArtifacts([{ id: 'one' }, { name: 'two' }])).sort(), ['one', 'two']);

const artifactResultEnvelope = normalizeArtifactResultEnvelope({
  schema: 'wp-codebox/artifact-result-envelope/v1',
  operation: 'import-artifact-bundle',
  status: 'created',
  artifactBundle: {
    kind: 'artifact-bundle',
    id: 'bundle-one',
    path: '/tmp/codebox-artifacts/bundle-one',
    digest: { algorithm: 'sha256', value: 'abc123' },
  },
  artifactRefs: [
    { kind: 'artifact-bundle', id: 'bundle-one', path: '/tmp/codebox-artifacts/bundle-one', digest: { value: 'abc123' } },
    { kind: 'artifact-log', path: '/tmp/codebox-artifacts/bundle-one/files/log.txt' },
  ],
  result: {
    typed_artifacts: {
      review: {
        kind: 'json',
        artifact_schema: 'example/review/v1',
        file_refs: [{ path: '/tmp/codebox-artifacts/bundle-one/files/review.json' }],
      },
    },
  },
});

assert.equal(artifactResultEnvelope.schema, 'wp-codebox/artifact-result-envelope/v1');
assert.equal(artifactResultEnvelope.success, true);
assert.equal(artifactResultEnvelope.artifactRefs.length, 2);
assert.equal(artifactResultEnvelope.artifactRefs[0].sha256, 'abc123');

const projectedEnvelope = artifactResultEnvelopeFromCodeboxResult({
  projections: [
    { kind: 'noop', schema: 'example/noop/v1' },
    { kind: 'artifact-result', schema: 'wp-codebox/artifact-result-envelope/v1', envelope: artifactResultEnvelope },
  ],
});
assert.equal(projectedEnvelope.operation, 'import-artifact-bundle');
assert.deepEqual(Object.keys(typedArtifactsFromCodeboxResult(projectedEnvelope)), ['review']);
console.log('wp-codebox artifact contract smoke passed');
