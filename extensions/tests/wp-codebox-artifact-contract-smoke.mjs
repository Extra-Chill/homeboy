#!/usr/bin/env node
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(repoRoot, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const {
  WP_CODEBOX_RUNTIME_ACCESS_SCHEMA,
  artifactResultEnvelopeFromCodeboxResult,
  artifactRoleFromCodeboxArtifact,
  allowArtifactRoleFallbackCompatibility,
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
const {
  AGENT_TASK_REQUEST_SCHEMA,
  WP_CODEBOX_BACKEND,
  agentTaskOutcomeFromCodeboxResult,
} = require(path.join(repoRoot, 'agent-runtimes/wp-codebox/lib/codebox-agent-task-executor'));

assert.equal(artifactPath('/tmp/artifacts/', '/files/transcript.json'), '/tmp/artifacts/files/transcript.json');
assert.equal(artifactPath('', 'files/transcript.json'), '');
assert.equal(artifactNameFromDeclaration({ id: 'transcript' }), 'transcript');
assert.equal(artifactRoleFromCodeboxArtifact({ kind: 'codebox-patch' }, { artifact_roles: { patch: ['codebox-patch'] } }), 'patch');
assert.equal(artifactRoleFromCodeboxArtifact({ path: '/tmp/files/changed-files.json' }), 'artifact');
assert.equal(artifactRoleFromCodeboxArtifact({ path: '/tmp/files/changed-files.json' }, { allowArtifactRoleFallbackCompatibility: true }), 'changed_files');
assert.equal(allowArtifactRoleFallbackCompatibility({}), false);
assert.equal(allowArtifactRoleFallbackCompatibility({ allowLegacyCodeboxResultCompatibility: true }), true);

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

const canonicalEnvelope = artifactResultEnvelopeFromCodeboxResult({
  artifact_result: artifactResultEnvelope,
});
assert.equal(canonicalEnvelope.operation, 'import-artifact-bundle');
assert.deepEqual(Object.keys(typedArtifactsFromCodeboxResult(canonicalEnvelope)), ['review']);

const projectedResult = {
  projections: [
    { kind: 'noop', schema: 'example/noop/v1' },
    { kind: 'artifact-result', schema: 'wp-codebox/artifact-result-envelope/v1', envelope: artifactResultEnvelope },
  ],
};
assert.equal(artifactResultEnvelopeFromCodeboxResult(projectedResult), null);
const projectedEnvelope = artifactResultEnvelopeFromCodeboxResult(projectedResult, { allowLegacyCodeboxResultCompatibility: true });
assert.equal(projectedEnvelope.operation, 'import-artifact-bundle');
assert.deepEqual(Object.keys(typedArtifactsFromCodeboxResult(projectedResult)), []);
assert.deepEqual(Object.keys(typedArtifactsFromCodeboxResult(projectedResult, { allowLegacyCodeboxResultCompatibility: true })), ['review']);
assert.equal(artifactResultEnvelopeFromCodeboxResult({ artifactResult: artifactResultEnvelope }), null);
assert.equal(
  artifactResultEnvelopeFromCodeboxResult({ artifactResult: artifactResultEnvelope }, { allowLegacyCodeboxResultCompatibility: true }).operation,
  'import-artifact-bundle'
);
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

const runtimeAccessResult = {
  artifact_result: normalizeArtifactResultEnvelope({
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    result: {
      outputs: {
        preview_materialization: {
          schema: 'homeboy/preview-materialization-evidence/v1',
          url: 'https://preview.example.test/',
          public_url: 'https://public.example.test/',
          site: {
            url: 'https://site.example.test/',
            admin_url: 'https://site.example.test/wp-admin/',
          },
          lease: { id: 'lease-1' },
          reviewer: { id: 'reviewer-1' },
          status: { state: 'ready' },
          refs: [{ kind: 'preview', uri: 'https://preview.example.test/', label: 'Preview' }],
        },
      },
    },
  }),
};
const runtimeAccessArtifacts = typedArtifactsFromCodeboxResult(runtimeAccessResult);
assert.equal(runtimeAccessArtifacts.runtime_access.artifact_schema, WP_CODEBOX_RUNTIME_ACCESS_SCHEMA);
assert.equal(runtimeAccessArtifacts.runtime_access.payload.schema, WP_CODEBOX_RUNTIME_ACCESS_SCHEMA);
assert.equal(runtimeAccessArtifacts.runtime_access.payload.preview_url, 'https://preview.example.test/');
assert.equal(runtimeAccessArtifacts.runtime_access.payload.public_url, 'https://public.example.test/');
assert.equal(runtimeAccessArtifacts.runtime_access.payload.site_url, 'https://site.example.test/');
assert.equal(runtimeAccessArtifacts.runtime_access.payload.admin_url, 'https://site.example.test/wp-admin/');
assert.deepEqual(runtimeAccessArtifacts.runtime_access.payload.lease, { id: 'lease-1' });
assert.deepEqual(runtimeAccessArtifacts.runtime_access.payload.reviewer, { id: 'reviewer-1' });
assert.deepEqual(runtimeAccessArtifacts.runtime_access.payload.status, { state: 'ready' });
assert.deepEqual(runtimeAccessArtifacts.runtime_access.payload.refs, [{ kind: 'preview', uri: 'https://preview.example.test/', label: 'Preview' }]);

const normalizedOutcome = agentTaskOutcomeFromCodeboxResult({
  schema: AGENT_TASK_REQUEST_SCHEMA,
  task_id: 'task-runtime-access',
  instructions: 'Report preview access.',
  executor: { backend: WP_CODEBOX_BACKEND },
}, runtimeAccessResult);
assert.equal(normalizedOutcome.outputs.typed_artifacts.runtime_access.payload.preview_url, 'https://preview.example.test/');
assert.equal(
  normalizedOutcome.typed_artifacts.find((artifact) => artifact.name === 'runtime_access')?.artifact_schema,
  WP_CODEBOX_RUNTIME_ACCESS_SCHEMA
);
assert.deepEqual(typedArtifactsFromCodeboxResult({
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
}), {});
assert.equal(typedArtifactsFromCodeboxResult({
  metadata: {
    agent_runtime: {
      result: {
        schema: 'wp-codebox/artifact-result-envelope/v1',
        outputs: {
          typed_artifacts: {
            legacy: { type: 'json', payload: { old: true } },
          },
        },
      },
    },
  },
}, { allowLegacyCodeboxResultCompatibility: true }).legacy.payload.old, true);

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
      fuzz_results: {
        schema: 'wp-codebox/fuzz-results/v1',
        component_id: 'plugin-under-test',
        scenarios: [{
          id: 'rest-cases',
          artifactRefs: [
            { path: 'files/fuzz/plugin/rest-cases-summary.json', kind: 'fuzz-rest-request-case-summary', sha256: 'summary-sha' },
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
      },
    },
  },
});

assert.equal(caseArtifactIndex.schema, 'wp-codebox/case-artifact-index/v1');
assert.equal(caseArtifactIndex.caseRefs.length, 2);
assert.deepEqual(caseArtifactIndex.caseRefs.map((ref) => ref.case_id), ['create-post', 'update-post']);
assert.deepEqual(caseArtifactIndex.caseRefs[0].artifactRefs, [
  { path: 'files/fuzz/plugin/rest-cases-summary.json', kind: 'fuzz-rest-request-case-summary', sha256: 'summary-sha' },
  { name: 'request', path: 'files/fuzz/create-post-request.json', kind: 'request-evidence' },
]);
assert.deepEqual(caseArtifactIndex.caseRefs[1].artifactRefs, [
  { path: 'files/fuzz/plugin/rest-cases-summary.json', kind: 'fuzz-rest-request-case-summary', sha256: 'summary-sha' },
]);
console.log('wp-codebox artifact contract smoke passed');
