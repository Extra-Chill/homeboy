'use strict';

const assert = require('node:assert/strict');
const runtimeAgentCi = require('..');
const {
  GATE_PLAN_SCHEMA,
  GATE_RESULT_SCHEMA,
  buildGatePlan,
  evaluateGatePlan,
  evaluateGateResults,
} = require('../lib/gate-plan-evaluator');

assert.equal(runtimeAgentCi.GATE_PLAN_SCHEMA, GATE_PLAN_SCHEMA);
assert.equal(runtimeAgentCi.GATE_RESULT_SCHEMA, GATE_RESULT_SCHEMA);
assert.equal(runtimeAgentCi.evaluateGatePlan, evaluateGatePlan);

const plan = buildGatePlan({ id: 'artifact_required', pass_when: [{ field: 'present', op: 'truthy' }] });
assert.equal(plan.schema, 'homeboy/gate-plan/v1');
assert.equal(plan.id, 'artifact_required');

let result = evaluateGatePlan({
  id: 'continue_when_clean',
  continue_when: [{ field: 'dirty', op: 'falsy' }],
}, { dirty: false });
assert.equal(result.schema, 'homeboy/gate-result/v1');
assert.equal(result.success, true);
assert.equal(result.action, 'continue');
assert.equal(result.status, 'passed');

result = evaluateGatePlan({
  id: 'stop_when_accepted',
  stop_when: [{ field: 'accepted', op: 'truthy', reason: 'accepted' }],
}, { accepted: true });
assert.equal(result.success, true);
assert.equal(result.action, 'stop');
assert.equal(result.status, 'stopped');
assert.equal(result.reason, 'accepted');

result = evaluateGatePlan({
  id: 'continue_declined',
  continue_when: [{ field: 'should_continue', op: 'truthy', reason: 'continuation_declined' }],
}, { should_continue: false });
assert.equal(result.success, true);
assert.equal(result.action, 'stop');
assert.equal(result.reason, 'continuation_declined');

result = evaluateGatePlan({
  id: 'allowed_status',
  pass_when: [{ field: 'status', op: 'in', values: ['succeeded'], failure_class: 'status.rejected', message: 'status rejected' }],
}, { status: 'failed' });
assert.equal(result.success, false);
assert.equal(result.action, 'fail');
assert.equal(result.status, 'failed');
assert.equal(result.failures[0].class, 'status.rejected');
assert.equal(result.message, 'status rejected');

const summary = evaluateGateResults([
  evaluateGatePlan({ id: 'first', pass_when: [{ field: 'ok', op: 'truthy' }] }, { ok: true }),
  result,
]);
assert.equal(summary.schema, 'homeboy/gate-result-summary/v1');
assert.equal(summary.success, false);
assert.equal(summary.action, 'fail');
assert.equal(summary.error, 'status rejected');

process.stdout.write('Gate plan evaluator checks passed\n');
