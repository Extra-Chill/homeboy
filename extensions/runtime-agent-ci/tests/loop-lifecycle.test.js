'use strict';

const assert = require('node:assert/strict');

const {
  assertLoopSuccess,
  loopEvidence,
  loopGateSummary,
  loopIteration,
  loopRun,
  withLoopGateResult,
} = require('../lib/loop-lifecycle.cjs');

const satisfied = assertLoopSuccess({
  results: {
    scenarios: [{
      id: 'fixture-flow',
      metadata: {
        job_status: 'completed',
        success_status: 'no_changes',
        completion_outcome: 'accepted',
      },
    }],
  },
  scenario_id: 'fixture-flow',
  success_requires_pr: true,
  success_completion_outcomes: ['accepted'],
});

assert.equal(satisfied.completion_outcome_satisfied, true);
assert.equal(satisfied.gate_result.success, true);
assert.equal(satisfied.gate_result.action, 'continue');

assert.throws(() => assertLoopSuccess({
  results: {
    scenarios: [{
      id: 'fixture-flow',
      metadata: { job_status: 'completed', success_status: 'no_changes' },
    }],
  },
  scenario_id: 'fixture-flow',
  success_requires_pr: true,
}), /expected opened PR, satisfied completion outcome, or allowed no-changes result/);

const commandGate = withLoopGateResult('verification_commands', { enabled: true, success: true, checks: [{ command: 'npm test' }] });
assert.equal(commandGate.gate_result.success, true);
assert.equal(loopGateSummary([commandGate.gate_result]).success, true);

const failedGate = withLoopGateResult('workspace_contract', { enabled: true, success: false, error: 'contract failed' });
assert.equal(failedGate.gate_result.success, false);
assert.equal(loopGateSummary([commandGate.gate_result, failedGate.gate_result]).success, false);

const evidence = loopEvidence({ kind: 'preview', url: 'https://example.test/preview/1', iteration: 2 });
const iteration = loopIteration({ loop_id: 'fixture-loop', iteration: 2, result: { status: 'succeeded' }, evidence_refs: [evidence], gate_result: commandGate.gate_result, accepted: true });
const run = loopRun({ loop_id: 'fixture-loop', status: 'completed', max_iterations: 3, iterations: [iteration], evidence: [evidence], gate_summary: loopGateSummary([commandGate.gate_result]) });

assert.equal(evidence.schema, 'homeboy/loop-evidence/v1');
assert.equal(iteration.schema, 'homeboy/loop-iteration/v1');
assert.equal(run.schema, 'homeboy/loop-run/v1');
assert.equal(run.iteration_count, 1);
assert.equal(run.gate_summary.success, true);

process.stdout.write('Loop lifecycle helpers behavior checks passed\n');
