'use strict';

const assert = require('node:assert/strict');

const {
  BOUNDED_PRODUCTION_LOOP_EVIDENCE_SCHEMA,
  BOUNDED_PRODUCTION_LOOP_ITERATION_SCHEMA,
  BOUNDED_PRODUCTION_LOOP_RESULT_SCHEMA,
  runBoundedProductionLoop,
} = require('../lib/bounded-production-loop-runner');

assert.equal(BOUNDED_PRODUCTION_LOOP_RESULT_SCHEMA, 'homeboy/bounded-production-loop-result/v1');
assert.equal(BOUNDED_PRODUCTION_LOOP_ITERATION_SCHEMA, 'homeboy/bounded-production-loop-iteration/v1');
assert.equal(BOUNDED_PRODUCTION_LOOP_EVIDENCE_SCHEMA, 'homeboy/bounded-production-loop-evidence/v1');

const stopsAfterPass = runBoundedProductionLoop({
  loopId: 'stops-after-pass',
  maxIterations: 3,
  executeIteration: () => ({
    status: 'accepted',
    artifacts: [{ name: 'build-output', path: 'artifacts/build.json' }],
    evidence_refs: [{ kind: 'preview', url: 'https://example.test/preview' }, { kind: 'publication', url: 'https://example.test/pull/1' }],
  }),
  artifactRequirements: ['build-output'],
  previewRequirement: true,
  publicationEvidenceRequirement: true,
});

assert.equal(stopsAfterPass.schema, BOUNDED_PRODUCTION_LOOP_RESULT_SCHEMA);
assert.equal(stopsAfterPass.status, 'succeeded');
assert.equal(stopsAfterPass.stop_reason, 'accepted');
assert.equal(stopsAfterPass.iteration_count, 1);
assert.equal(stopsAfterPass.iterations[0].schema, BOUNDED_PRODUCTION_LOOP_ITERATION_SCHEMA);
assert.equal(stopsAfterPass.evidence_envelope.schema, BOUNDED_PRODUCTION_LOOP_EVIDENCE_SCHEMA);
assert.equal(stopsAfterPass.validation_failures.length, 0);

const stopsAtMaxIterations = runBoundedProductionLoop({
  loopId: 'stops-at-max',
  maxIterations: 2,
  executeIteration: () => ({ status: 'needs_revision' }),
});

assert.equal(stopsAtMaxIterations.status, 'failed');
assert.equal(stopsAtMaxIterations.stop_reason, 'max_iterations_reached');
assert.equal(stopsAtMaxIterations.iteration_count, 2);

const missingPreview = runBoundedProductionLoop({
  loopId: 'missing-preview',
  maxIterations: 1,
  executeIteration: () => ({ status: 'accepted' }),
  previewRequirement: true,
});

assert.equal(missingPreview.status, 'failed');
assert.equal(missingPreview.validation_failures[0].code, 'missing_required_evidence');
assert.equal(missingPreview.validation_failures[0].requirement.kind, 'preview');

const missingPublication = runBoundedProductionLoop({
  loopId: 'missing-publication',
  maxIterations: 1,
  executeIteration: () => ({ status: 'accepted', evidence_refs: [{ kind: 'preview', url: 'https://example.test/preview' }] }),
  previewRequirement: true,
  publicationEvidenceRequirement: true,
});

assert.equal(missingPublication.status, 'failed');
assert.equal(missingPublication.validation_failures[0].requirement.kind, 'publication');

const optionalEvidenceMissing = runBoundedProductionLoop({
  loopId: 'optional-evidence',
  maxIterations: 1,
  executeIteration: () => ({ status: 'accepted' }),
  evidenceRequirements: [{ kind: 'preview', optional: true }],
});

assert.equal(optionalEvidenceMissing.status, 'succeeded');
assert.equal(optionalEvidenceMissing.evidence_envelope.optional_evidence.length, 1);
assert.equal(optionalEvidenceMissing.evidence_envelope.required_evidence.length, 0);

const twoIterationRevision = runBoundedProductionLoop({
  loopId: 'needs-revision-then-accepted',
  maxIterations: 3,
  buildIteration: ({ iteration }) => ({ iteration }),
  executeIteration: ({ task }) => task.iteration < 2
    ? { status: 'needs_revision', evidence_refs: [{ kind: 'review', url: 'https://example.test/review/1' }] }
    : { status: 'accepted', evidence_refs: [{ kind: 'publication', url: 'https://example.test/pull/2' }] },
  projectFinalState: ({ iterations }) => ({ accepted_iteration: iterations.find((iteration) => iteration.accepted)?.iteration || null }),
  publicationEvidenceRequirement: true,
});

assert.equal(twoIterationRevision.status, 'succeeded');
assert.equal(twoIterationRevision.iteration_count, 2);
assert.equal(twoIterationRevision.iterations[0].accepted, false);
assert.equal(twoIterationRevision.iterations[0].validation_failures[0].requirement.kind, 'publication');
assert.equal(twoIterationRevision.iterations[1].accepted, true);
assert.equal(twoIterationRevision.final_state.accepted_iteration, 2);

let repairCalls = 0;
let fanoutCalls = 0;
const hookRun = runBoundedProductionLoop({
  loopId: 'policy-hooks',
  maxIterations: 1,
  executeIteration: () => ({ status: 'accepted' }),
  repairPolicy: ({ accepted }) => {
    repairCalls += 1;
    return { required: !accepted };
  },
  fanoutPolicy: ({ accepted }) => {
    fanoutCalls += 1;
    return { planned: accepted };
  },
});

assert.equal(repairCalls, 1);
assert.equal(fanoutCalls, 1);
assert.deepEqual(hookRun.iterations[0].repair, { required: false });
assert.deepEqual(hookRun.iterations[0].fanout, { planned: true });

process.stdout.write('Bounded production loop runner behavior checks passed\n');
