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
  caseArtifactIndexFromCodeboxResult,
  normalizeArtifactResultEnvelope,
  normalizeCaseArtifactIndex,
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
assert.deepEqual(typedArtifactsFromCodeboxResult({
  artifact_result: artifactResultEnvelope,
  metadata: {
    agent_runtime: {
      result: {
        outputs: {
          typed_artifacts: {
            legacy: { type: 'json', payload: { old: true } },
          },
        },
      },
    },
  },
}).review.artifact_schema, 'example/review/v1');
assert.equal(Object.hasOwn(typedArtifactsFromCodeboxResult({
  artifact_result: artifactResultEnvelope,
  metadata: {
    agent_runtime: {
      result: {
        outputs: {
          typed_artifacts: {
            legacy: { type: 'json', payload: { old: true } },
          },
        },
      },
    },
  },
}), 'legacy'), false);
assert.equal(typedArtifactsFromCodeboxResult({
  metadata: {
    agent_runtime: {
      result: {
        outputs: {
          typed_artifacts: {
            legacy: { type: 'json', payload: { old: true } },
          },
        },
      },
    },
  },
}).legacy.payload.old, true);

assert.deepEqual(normalizeCaseArtifactIndex({
  case_refs: [{
    componentId: 'component-one',
    scenarioId: 'scenario-one',
    caseId: 'case-one',
    artifact_refs: [
      { path: 'files/case-one.json', kind: 'case-evidence', digest: { value: 'sha-one' } },
      { path: 'files/case-one.json', kind: 'case-evidence', digest: { value: 'sha-one' } },
      'files/case-one.log',
    ],
  }],
}), {
  schema: 'wp-codebox/case-artifact-index/v1',
  caseRefs: [{
    component_id: 'component-one',
    scenario_id: 'scenario-one',
    case_id: 'case-one',
    artifactRefs: [
      { path: 'files/case-one.json', kind: 'case-evidence', sha256: 'sha-one' },
      { path: 'files/case-one.log' },
    ],
  }],
});

const caseArtifactIndex = caseArtifactIndexFromCodeboxResult({
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    result: {
      benchmark_artifacts: {
        schema: 'wp-codebox/benchmark-artifacts/v1',
        results: [{
          component_id: 'plugin-under-test',
          scenarios: [{
            id: 'fuzz-rest-cases',
            artifactRefs: [
              { path: 'files/bench/plugin/fuzz-rest-cases-summary.json', kind: 'benchmark-rest-request-case-summary', sha256: 'summary-sha' },
            ],
            cases: [{
              id: 'create-post',
              status: 201,
              artifacts: {
                request: { path: 'files/fuzz/create-post-request.json', kind: 'request-evidence' },
              },
            }],
            steps: [{
              type: 'rest-request',
              rest_request_case_index: 1,
              case_id: 'update-post',
              status: 200,
            }],
          }],
        }],
      },
    },
  },
});

assert.equal(caseArtifactIndex.schema, 'wp-codebox/case-artifact-index/v1');
assert.equal(caseArtifactIndex.caseRefs.length, 2);
assert.deepEqual(caseArtifactIndex.caseRefs.map((ref) => ref.case_id), ['create-post', 'update-post']);
assert.deepEqual(caseArtifactIndex.caseRefs[0].artifactRefs, [
  { path: 'files/bench/plugin/fuzz-rest-cases-summary.json', kind: 'benchmark-rest-request-case-summary', sha256: 'summary-sha' },
  { name: 'request', path: 'files/fuzz/create-post-request.json', kind: 'request-evidence' },
]);
assert.deepEqual(caseArtifactIndex.caseRefs[1].artifactRefs, [
  { path: 'files/bench/plugin/fuzz-rest-cases-summary.json', kind: 'benchmark-rest-request-case-summary', sha256: 'summary-sha' },
]);
console.log('wp-codebox artifact contract smoke passed');
