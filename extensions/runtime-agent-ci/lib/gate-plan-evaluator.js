'use strict';

const GATE_PLAN_SCHEMA = 'homeboy/gate-plan/v1';
const GATE_RESULT_SCHEMA = 'homeboy/gate-result/v1';

function buildGatePlan(plan = {}) {
  return {
    schema: GATE_PLAN_SCHEMA,
    id: plan.id || plan.name || 'gate',
    label: plan.label || plan.description || plan.id || plan.name || 'Gate',
    enabled: plan.enabled !== false,
    mode: plan.mode || 'fail',
    pass_when: normalizeArray(plan.pass_when || plan.passWhen),
    fail_when: normalizeArray(plan.fail_when || plan.failWhen),
    stop_when: normalizeArray(plan.stop_when || plan.stopWhen),
    continue_when: normalizeArray(plan.continue_when || plan.continueWhen),
    data: isPlainObject(plan.data) ? plan.data : {},
  };
}

function evaluateGatePlan(planInput = {}, context = {}) {
  const plan = buildGatePlan(planInput);
  if (!plan.enabled) {
    return gateResult(plan, { status: 'skipped', action: 'continue', success: true, reason: 'gate_disabled' });
  }

  const failed = [
    ...plan.fail_when.filter((condition) => conditionMatches(condition, context)),
    ...plan.pass_when.filter((condition) => !conditionMatches(condition, context)),
  ];
  if (failed.length > 0) {
    const failure = failed[0];
    return gateResult(plan, {
      status: 'failed',
      action: 'fail',
      success: false,
      reason: conditionReason(failure, 'gate_failed'),
      failures: failed.map((condition) => gateFailure(condition, context)),
    });
  }

  const stop = plan.stop_when.find((condition) => conditionMatches(condition, context));
  if (stop) {
    return gateResult(plan, { status: 'stopped', action: 'stop', success: true, reason: conditionReason(stop, 'stop_criteria_satisfied') });
  }

  if (plan.continue_when.length > 0) {
    const declined = plan.continue_when.find((condition) => !conditionMatches(condition, context));
    if (declined) {
      return gateResult(plan, { status: 'stopped', action: 'stop', success: true, reason: conditionReason(declined, 'continuation_declined') });
    }
  }

  return gateResult(plan, { status: 'passed', action: 'continue', success: true, reason: 'gate_passed' });
}

function evaluateGateResults(results = {}) {
  const gateResults = Array.isArray(results) ? results : Object.values(results).filter(Boolean);
  const enabled = gateResults.filter((result) => result.enabled !== false && result.status !== 'skipped');
  const failure = enabled.find((result) => result.success === false || result.action === 'fail');
  const stop = enabled.find((result) => result.action === 'stop');
  return {
    schema: 'homeboy/gate-result-summary/v1',
    success: !failure,
    action: failure ? 'fail' : stop ? 'stop' : 'continue',
    reason: (failure || stop)?.reason || 'gate_passed',
    error: failure?.message || failure?.reason || '',
    gate_count: gateResults.length,
    enabled_gate_count: enabled.length,
    results: gateResults,
  };
}

function gateResult(plan, result = {}) {
  const failures = normalizeArray(result.failures);
  return {
    schema: GATE_RESULT_SCHEMA,
    gate_plan_schema: GATE_PLAN_SCHEMA,
    id: plan.id,
    label: plan.label,
    enabled: plan.enabled !== false,
    status: result.status || 'passed',
    action: result.action || 'continue',
    success: result.success !== false,
    reason: result.reason || '',
    message: result.message || result.error || failures[0]?.message || '',
    failures,
    data: { ...plan.data, ...(isPlainObject(result.data) ? result.data : {}) },
  };
}

function gateFailure(condition, context = {}) {
  return {
    class: condition.class || condition.failure_class || 'gate.condition_failed',
    message: condition.message || `Gate condition failed: ${condition.field || condition.path || '(context)'}`,
    data: {
      field: condition.field || condition.path || '',
      op: condition.op || 'truthy',
      expected: condition.value !== undefined ? condition.value : condition.values,
      actual: getPath(context, condition.field || condition.path || ''),
    },
  };
}

function conditionMatches(conditionInput = {}, context = {}) {
  const condition = isPlainObject(conditionInput) ? conditionInput : { value: conditionInput, op: 'truthy' };
  const actual = condition.field || condition.path ? getPath(context, condition.field || condition.path) : context;
  const op = condition.op || (Object.prototype.hasOwnProperty.call(condition, 'values') ? 'in' : Object.prototype.hasOwnProperty.call(condition, 'value') ? 'equals' : 'truthy');
  switch (op) {
    case 'equals':
      return actual === condition.value;
    case 'not_equals':
      return actual !== condition.value;
    case 'in':
      return normalizeArray(condition.values).includes(actual);
    case 'not_in':
      return !normalizeArray(condition.values).includes(actual);
    case 'truthy':
      return Boolean(actual);
    case 'falsy':
      return !actual;
    case 'present':
      return actual !== undefined && actual !== null && actual !== '';
    case 'missing':
      return actual === undefined || actual === null || actual === '';
    case 'lte':
      return Number(actual) <= Number(condition.value);
    case 'gte':
      return Number(actual) >= Number(condition.value);
    default:
      throw new Error(`Unsupported gate condition op: ${op}`);
  }
}

function conditionReason(condition = {}, fallback) {
  return condition.reason || condition.class || condition.failure_class || fallback;
}

function getPath(value, path) {
  if (!path) {
    return value;
  }
  return String(path).split('.').filter(Boolean).reduce((current, segment) => {
    if (!current || typeof current !== 'object') {
      return undefined;
    }
    return current[segment];
  }, value);
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function isPlainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
  GATE_PLAN_SCHEMA,
  GATE_RESULT_SCHEMA,
  buildGatePlan,
  evaluateGatePlan,
  evaluateGateResults,
  gateResult,
};
