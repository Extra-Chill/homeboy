#!/usr/bin/env node
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
  artifactNameFromDeclaration,
  artifactPath,
  normalizeTypedArtifactEntry,
  normalizeTypedArtifacts,
  typedArtifactFileRefs,
} = require(path.join(repoRoot, 'agent-runtimes/wp-codebox/lib/codebox-artifact-contract'));

assert.equal(artifactPath('/tmp/artifacts/', '/files/transcript.json'), '/tmp/artifacts/files/transcript.json');
assert.equal(artifactPath('', 'files/transcript.json'), '');
assert.equal(artifactNameFromDeclaration({ id: 'transcript' }), 'transcript');

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
console.log('wp-codebox artifact contract smoke passed');
