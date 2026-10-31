#!/usr/bin/env node
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const runtimeAgentCi = require(path.join(repoRoot, 'runtime-agent-ci/generic-orchestration'));
const validator = require(path.join(repoRoot, 'runtime-agent-ci/lib/controller-loop-proof-validator'));

assert.equal(typeof runtimeAgentCi.validateControllerLoopProof, 'function');
assert.equal(runtimeAgentCi.validateControllerLoopProof, validator.validateControllerLoopProof);
assert.equal(typeof runtimeAgentCi.assertControllerLoopProof, 'function');
assert.equal(typeof runtimeAgentCi.localOnlyReviewerFacingRef, 'function');

const baseSpec = {
  schema: 'example/controller-loop-spec/v1',
  artifacts: [
    { id: 'run-log', required: true, durable_url_required: true },
    { id: 'debug-bundle', required: false },
  ],
  policy: {
    max_iterations: 3,
    allowed_stop_reasons: ['accepted'],
    event_lineage_required: true,
  },
};
const baseProof = {
  artifacts: [{ id: 'run-log', url: 'https://example.test/artifacts/run-log' }],
  evidence: [{ id: 'review', kind: 'summary', url: 'https://example.test/review' }],
  iterations: [
    { iteration: 1, event_id: 'event-1', stop: { stop: false } },
    { iteration: 2, event_id: 'event-2', stop: { stop: true, reason: 'accepted' } },
  ],
  events: [
    { id: 'event-1' },
    { id: 'event-2', parent_id: 'event-1' },
  ],
};

let report = validator.validateControllerLoopProof({ spec: baseSpec, proof: baseProof });
assert.equal(report.valid, true);
assert.equal(report.summary.iteration_count, 2);
assert.equal(report.summary.stop_reason, 'accepted');
assert.deepEqual(report.artifact_results.map((result) => [result.id, result.required, result.present]), [
  ['run-log', true, true],
  ['debug-bundle', false, false],
]);

report = validator.validateControllerLoopProof({
  spec: baseSpec,
  proof: { ...baseProof, artifacts: [] },
});
assert.equal(report.valid, false);
assert.equal(hasFailure(report, 'artifact.required_missing'), true);

report = validator.validateControllerLoopProof({
  spec: baseSpec,
  proof: { ...baseProof, artifacts: [{ id: 'run-log', url: 'http://localhost:8888/run-log' }] },
});
assert.equal(report.valid, false);
assert.equal(hasFailure(report, 'artifact.local_reviewer_evidence'), true);

for (const localRef of [
  'http://127.0.0.1:8888/run-log',
  'file:///tmp/run-log',
  '/tmp/run-log',
  '/Users/chris/run-log',
  './run-log',
  '../run-log',
  'run-log.json',
]) {
  report = validator.validateControllerLoopProof({
    spec: baseSpec,
    proof: { ...baseProof, artifacts: [{ id: 'run-log', path: localRef }] },
  });
  assert.equal(report.valid, false, `${localRef} should be rejected`);
  assert.equal(hasFailure(report, 'artifact.local_reviewer_evidence'), true, `${localRef} should report local reviewer evidence`);
}

report = validator.validateControllerLoopProof({
  spec: baseSpec,
  proof: { ...baseProof, artifacts: [{ id: 'run-log', public_url: 'https://example.test/artifacts/run-log', path: '/tmp/private-run-log' }] },
});
assert.equal(report.valid, true);

report = validator.validateControllerLoopProof({
  spec: baseSpec,
  proof: { ...baseProof, artifacts: [{ id: 'run-log', url: 'file:///tmp/private-run-log', public_url: 'https://example.test/artifacts/run-log' }] },
});
assert.equal(report.valid, false);
assert.equal(hasFailure(report, 'artifact.local_reviewer_evidence'), true);

report = validator.validateControllerLoopProof({
  spec: { ...baseSpec, policy: { ...baseSpec.policy, preview_required: true } },
  proof: baseProof,
});
assert.equal(report.valid, false);
assert.equal(hasFailure(report, 'preview.required_missing'), true);

report = validator.validateControllerLoopProof({
  spec: { ...baseSpec, policy: { ...baseSpec.policy, preview_required: true } },
  proof: { ...baseProof, evidence: [...baseProof.evidence, { id: 'preview-url', kind: 'preview', url: 'https://preview.example.test/run' }] },
});
assert.equal(report.valid, true);

report = validator.validateControllerLoopProof({
  spec: { ...baseSpec, policy: { ...baseSpec.policy, pr_required: true } },
  proof: baseProof,
});
assert.equal(report.valid, false);
assert.equal(hasFailure(report, 'publication.required_missing'), true);

report = validator.validateControllerLoopProof({
  spec: { ...baseSpec, policy: { ...baseSpec.policy, pr_required: true } },
  proof: { ...baseProof, evidence: [...baseProof.evidence, { id: 'change-review', kind: 'pull_request', url: 'https://example.test/pull/1' }] },
});
assert.equal(report.valid, true);

report = validator.validateControllerLoopProof({
  spec: baseSpec,
  proof: {
    ...baseProof,
    iterations: [
      ...baseProof.iterations,
      { iteration: 3, event_id: 'event-3' },
      { iteration: 4, event_id: 'event-4', stop: { reason: 'timed_out' } },
    ],
    events: [...baseProof.events, { id: 'event-3', parent_id: 'event-2' }, { id: 'event-4', parent_id: 'event-3' }],
  },
});
assert.equal(report.valid, false);
assert.equal(hasFailure(report, 'loop.max_iterations_exceeded'), true);
assert.equal(hasFailure(report, 'loop.stop_reason_rejected'), true);

report = validator.validateControllerLoopProof({
  spec: baseSpec,
  proof: {
    ...baseProof,
    iterations: [{ iteration: 1, event_id: 'missing-event', stop: { reason: 'accepted' } }],
  },
});
assert.equal(report.valid, false);
assert.equal(hasFailure(report, 'event_lineage.iteration_event_unknown'), true);

function hasFailure(validationReport, className) {
  return validationReport.failures.some((item) => item.class === className);
}

console.log('Controller loop proof validator checks passed');
