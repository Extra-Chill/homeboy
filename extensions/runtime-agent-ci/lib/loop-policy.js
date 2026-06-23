'use strict';

const LOOP_POLICY_SCHEMA = 'homeboy/loop-policy/v1';
const LOOP_POLICY_STATUS_SCHEMA = 'homeboy/loop-policy-status/v1';
const CONTINUE = 'continue';

function normalizeLoopPolicy(input = {}, options = {}) {
  const raw = optionalObject(input.loop_policy || input.loopPolicy || input.loop || input);
  const defaultMode = options.defaultMode || options.default_mode || '';
  const maxRevolutions = positiveInteger(raw.max_revolutions ?? raw.maxRevolutions ?? raw.max_iterations ?? raw.maxIterations ?? input.max_revolutions ?? input.maxRevolutions ?? input.max_iterations ?? input.maxIterations);
  const durationMs = nonNegativeInteger(raw.duration_ms ?? raw.durationMs ?? input.duration_ms ?? input.durationMs);
  const deadlineAt = normalizeDeadline(raw.deadline_at ?? raw.deadlineAt ?? input.deadline_at ?? input.deadlineAt);
  const mode = normalizeMode(raw.mode || input.mode || defaultMode || inferredMode({ maxRevolutions, durationMs, deadlineAt }));
  return {
    schema: LOOP_POLICY_SCHEMA,
    mode,
    max_revolutions: mode === 'count' ? (maxRevolutions || positiveInteger(options.defaultMaxRevolutions ?? options.default_max_revolutions) || 1) : 0,
    duration_ms: durationMs,
    deadline_at: deadlineAt,
    cancellation_signal: raw.cancellation_signal || raw.cancellationSignal || input.cancellation_signal || input.cancellationSignal || options.cancellation_signal || options.cancellationSignal || null,
    cancelled: Boolean(raw.cancelled || input.cancelled || options.cancelled),
  };
}

function evaluateLoopPolicy(policyInput, context = {}) {
  const policy = policyInput?.schema === LOOP_POLICY_SCHEMA ? policyInput : normalizeLoopPolicy(policyInput);
  if (policy.cancelled || context.cancelled || signalCancelled(policy.cancellation_signal) || signalCancelled(context.cancellation_signal || context.cancellationSignal)) {
    return status('cancelled', context);
  }
  const now = numericNow(context.now);
  const deadlineAt = policy.deadline_at || normalizeDeadline(context.deadline_at || context.deadlineAt);
  if (deadlineAt > 0 && now >= deadlineAt) {
    return status('deadline_reached', context);
  }
  const durationMs = policy.duration_ms || nonNegativeInteger(context.duration_ms || context.durationMs);
  const startedAt = normalizeDeadline(context.started_at || context.startedAt || context.start_at || context.startAt);
  if (durationMs > 0 && startedAt > 0 && now - startedAt >= durationMs) {
    return status('duration_elapsed', context);
  }
  const completedRevolutions = nonNegativeInteger(context.completed_revolutions ?? context.completedRevolutions ?? context.iteration ?? context.revolution);
  if (policy.mode === 'count' && policy.max_revolutions > 0 && completedRevolutions >= policy.max_revolutions) {
    return status('max_revolutions_reached', context);
  }
  return status(CONTINUE, context);
}

function loopPolicyMaxRevolutions(policyInput, options = {}) {
  const policy = policyInput?.schema === LOOP_POLICY_SCHEMA ? policyInput : normalizeLoopPolicy(policyInput);
  if (policy.mode === 'count') {
    return policy.max_revolutions;
  }
  if ((options.nonCountMaxRevolutions ?? options.non_count_max_revolutions) === Number.POSITIVE_INFINITY) {
    return Number.POSITIVE_INFINITY;
  }
  const maxRevolutions = positiveInteger(options.nonCountMaxRevolutions ?? options.non_count_max_revolutions, 0);
  if (maxRevolutions > 0) {
    return maxRevolutions;
  }
  if (options.requireNonCountMaxRevolutions || options.require_non_count_max_revolutions) {
    throw new Error('Synchronous duration, deadline, and indefinite loop policies require max_synchronous_revolutions so they do not silently run once. Set max_synchronous_revolutions to a positive integer, or use a durable loop for unbounded execution.');
  }
  return 1;
}

function status(reason, context = {}) {
  return {
    schema: LOOP_POLICY_STATUS_SCHEMA,
    stop: reason !== CONTINUE,
    reason,
    stop_reason: reason,
    completed_revolutions: nonNegativeInteger(context.completed_revolutions ?? context.completedRevolutions ?? context.iteration ?? context.revolution),
  };
}

function inferredMode({ maxRevolutions, durationMs, deadlineAt }) {
  if (maxRevolutions > 0) {
    return 'count';
  }
  if (durationMs > 0 || deadlineAt > 0) {
    return 'duration';
  }
  return 'indefinite';
}

function normalizeMode(value) {
  return ['count', 'duration', 'indefinite'].includes(value) ? value : 'indefinite';
}

function normalizeDeadline(value) {
  if (value instanceof Date) {
    return value.getTime();
  }
  const numeric = nonNegativeInteger(value);
  if (numeric > 0) {
    return numeric;
  }
  if (typeof value === 'string' && value.trim() !== '') {
    const parsedDate = Date.parse(value);
    if (Number.isFinite(parsedDate)) {
      return parsedDate;
    }
  }
  return 0;
}

function numericNow(now) {
  if (typeof now === 'function') {
    return numericNow(now());
  }
  return nonNegativeInteger(now) || Date.now();
}

function signalCancelled(signal) {
  return Boolean(signal && (signal.aborted || signal.cancelled));
}

function positiveInteger(value, fallback = 0) {
  const parsed = Number.parseInt(value ?? '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function nonNegativeInteger(value) {
  const parsed = Number.parseInt(value ?? '', 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : 0;
}

function optionalObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

module.exports = {
  CONTINUE,
  LOOP_POLICY_SCHEMA,
  LOOP_POLICY_STATUS_SCHEMA,
  evaluateLoopPolicy,
  loopPolicyMaxRevolutions,
  normalizeLoopPolicy,
};
