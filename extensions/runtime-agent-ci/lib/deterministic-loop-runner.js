'use strict';

const DETERMINISTIC_LOOP_RESULT_SCHEMA = 'homeboy/deterministic-loop-result/v1';
const DETERMINISTIC_LOOP_ITERATION_SCHEMA = 'homeboy/deterministic-loop-iteration/v1';
const DETERMINISTIC_LOOP_ARTIFACT_SCHEMA = 'homeboy/deterministic-loop-artifact/v1';
const DETERMINISTIC_LOOP_DURABLE_STATE_SCHEMA = 'homeboy/deterministic-loop-durable-state/v1';
const DETERMINISTIC_LOOP_CHECKPOINT_SCHEMA = 'homeboy/deterministic-loop-checkpoint/v1';

function runDeterministicLoop(options = {}) {
  const loopId = options.loopId || options.loop_id || 'deterministic-loop';
  const execute = requiredFunction(options.execute || options.executeIteration || options.execute_iteration, 'execute');
  const buildIteration = options.buildIteration || options.build_iteration || defaultBuildIteration;
  const reconcile = options.reconcile || defaultReconcile;
  const shouldRetry = options.shouldRetry || options.should_retry || defaultShouldRetry;
  const stopCriteria = options.stopCriteria || options.stop_criteria || options.shouldStop || options.should_stop || defaultStopCriteria;
  const maxIterations = positiveInteger(options.maxIterations || options.max_iterations, 1);
  const maxAttempts = positiveInteger(options.maxAttempts || options.max_attempts || options.retry?.max_attempts, 1);
  let state = clonePlainObject(options.state || options.initialState || options.initial_state || {});
  const iterations = [];

  for (let iterationIndex = 0; iterationIndex < maxIterations; iterationIndex += 1) {
    const iteration = iterationIndex + 1;
    const input = buildIteration({ loop_id: loopId, iteration, state, iterations });
    let outcome;
    let error;
    let attempt = 0;
    let retryDecision = false;

    do {
      attempt += 1;
      error = null;
      try {
        outcome = execute({ loop_id: loopId, iteration, attempt, input, state, iterations });
      } catch (caught) {
        error = caught;
        outcome = errorOutcome(caught);
      }
      retryDecision = attempt < maxAttempts && Boolean(shouldRetry({
        loop_id: loopId,
        iteration,
        attempt,
        input,
        outcome,
        error,
        state,
        iterations,
      }));
    } while (retryDecision);

    const artifacts = normalizeArtifactRecords(outcome?.artifacts || outcome?.metadata?.artifacts, { loopId, iteration });
    const nextState = reconcile({
      loop_id: loopId,
      iteration,
      attempt,
      input,
      outcome,
      error,
      artifacts,
      state,
      iterations,
    });
    if (isPlainObject(nextState)) {
      state = nextState;
    }

    const stop = normalizeStopDecision(evaluateStopCriteria(stopCriteria, {
      loop_id: loopId,
      iteration,
      attempt,
      input,
      outcome,
      error,
      artifacts,
      state,
      iterations,
    }));
    const record = {
      schema: DETERMINISTIC_LOOP_ITERATION_SCHEMA,
      loop_id: loopId,
      iteration,
      attempt,
      input,
      outcome,
      artifacts,
      state,
      stop,
    };
    iterations.push(record);
    if (stop.stop) {
      break;
    }
  }

  return {
    schema: DETERMINISTIC_LOOP_RESULT_SCHEMA,
    loop_id: loopId,
    status: loopStatus(iterations),
    state,
    iterations,
  };
}

function createDurableDeterministicLoop(options = {}) {
  const loopId = options.loopId || options.loop_id || 'deterministic-loop';
  const submit = requiredFunction(options.submitIteration || options.submit_iteration || options.submit, 'submitIteration');
  const poll = requiredFunction(options.pollIteration || options.poll_iteration || options.poll, 'pollIteration');
  const buildIteration = options.buildIteration || options.build_iteration || defaultBuildIteration;
  const reconcile = options.reconcile || defaultReconcile;
  const shouldRetry = options.shouldRetry || options.should_retry || defaultShouldRetry;
  const stopCriteria = options.stopCriteria || options.stop_criteria || options.shouldStop || options.should_stop || defaultStopCriteria;
  const maxIterations = positiveInteger(options.maxIterations || options.max_iterations, 1);
  const maxAttempts = positiveInteger(options.maxAttempts || options.max_attempts || options.retry?.max_attempts, 1);
  const timeoutMs = nonNegativeInteger(options.timeoutMs || options.timeout_ms || options.timeout?.ms, 0);
  const backoffMs = nonNegativeInteger(options.backoffMs || options.backoff_ms || options.backoff?.ms || options.retry?.backoff_ms, 0);
  const now = typeof options.now === 'function' ? options.now : () => Date.now();
  const initialState = clonePlainObject(options.state || options.initialState || options.initial_state || {});

  return {
    submitIteration(state = {}) {
      return submitDurableIteration({
        loopId,
        state: normalizeDurableState(state, { loopId, initialState }),
        submit,
        buildIteration,
        maxIterations,
        maxAttempts,
        timeoutMs,
        now,
      });
    },
    pollIteration(state = {}) {
      return pollDurableIteration({
        loopId,
        state: normalizeDurableState(state, { loopId, initialState }),
        submit,
        poll,
        reconcile,
        shouldRetry,
        stopCriteria,
        maxIterations,
        maxAttempts,
        timeoutMs,
        backoffMs,
        now,
      });
    },
    resume(state = {}) {
      const durableState = normalizeDurableState(state, { loopId, initialState });
      if (!durableState.current) {
        return this.submitIteration(durableState);
      }
      return this.pollIteration(durableState);
    },
  };
}

function submitDurableIteration({ loopId, state, submit, buildIteration, maxIterations, maxAttempts, timeoutMs, now }) {
  if (state.done) {
    return state;
  }
  if (state.current) {
    return state;
  }
  const iteration = state.iterations.length + 1;
  if (iteration > maxIterations) {
    return completeDurableState(state, 'failed', 'max_iterations_reached');
  }
  const attempt = 1;
  const input = buildIteration({ loop_id: loopId, iteration, state: state.state, iterations: state.iterations });
  const submitted = submit({ loop_id: loopId, iteration, attempt, input, state: state.state, iterations: state.iterations });
  const submittedAt = now();
  const current = {
    loop_id: loopId,
    iteration,
    attempt,
    input,
    token: submitted?.token || submitted?.id || submitted?.job_id || '',
    submitted,
    submitted_at: submittedAt,
    deadline_at: timeoutMs > 0 ? submittedAt + timeoutMs : 0,
    max_attempts: maxAttempts,
  };
  return checkpointState({ ...state, current }, 'submitted', { iteration, attempt, token: current.token });
}

function pollDurableIteration({ loopId, state, submit, poll, reconcile, shouldRetry, stopCriteria, maxIterations, maxAttempts, timeoutMs, backoffMs, now }) {
  if (state.done || !state.current) {
    return state;
  }
  const current = state.current;
  const polledAt = now();
  const timedOut = current.deadline_at > 0 && polledAt >= current.deadline_at;
  const pollResult = timedOut ? { status: 'timed_out', outcome: { status: 'failed', summary: 'Iteration timed out.' } } : poll({
    loop_id: loopId,
    iteration: current.iteration,
    attempt: current.attempt,
    input: current.input,
    token: current.token,
    submitted: current.submitted,
    state: state.state,
    iterations: state.iterations,
  });
  const pollStatus = pollResult?.status || pollResult?.state || '';
  if (!timedOut && ['queued', 'running', 'pending', 'submitted'].includes(pollStatus)) {
    return checkpointState(state, 'polled', { iteration: current.iteration, attempt: current.attempt, status: pollStatus });
  }

  const error = timedOut ? new Error('Iteration timed out.') : null;
  const outcome = pollResult?.outcome || pollResult?.result || pollResult || errorOutcome(error);
  const retry = current.attempt < maxAttempts && Boolean(shouldRetry({
    loop_id: loopId,
    iteration: current.iteration,
    attempt: current.attempt,
    input: current.input,
    outcome,
    error,
    state: state.state,
    iterations: state.iterations,
  }));
  if (retry) {
    const nextAttempt = current.attempt + 1;
    const submittedAt = polledAt + backoffMs;
    const submitted = submit({
      loop_id: loopId,
      iteration: current.iteration,
      attempt: nextAttempt,
      input: current.input,
      state: state.state,
      iterations: state.iterations,
      retry_after_ms: backoffMs,
      previous_outcome: outcome,
    });
    const nextCurrent = {
      ...current,
      attempt: nextAttempt,
      token: submitted?.token || submitted?.id || submitted?.job_id || '',
      submitted,
      submitted_at: submittedAt,
      deadline_at: timeoutMs > 0 ? submittedAt + timeoutMs : 0,
    };
    return checkpointState({ ...state, current: nextCurrent }, 'retry_scheduled', { iteration: current.iteration, attempt: nextAttempt });
  }

  const artifacts = normalizeArtifactRecords(outcome?.artifacts || outcome?.metadata?.artifacts, { loopId, iteration: current.iteration });
  const nextState = reconcile({
    loop_id: loopId,
    iteration: current.iteration,
    attempt: current.attempt,
    input: current.input,
    outcome,
    error,
    artifacts,
    state: state.state,
    iterations: state.iterations,
  });
  const reconciledState = isPlainObject(nextState) ? nextState : state.state;
  const stop = normalizeStopDecision(evaluateStopCriteria(stopCriteria, {
    loop_id: loopId,
    iteration: current.iteration,
    attempt: current.attempt,
    input: current.input,
    outcome,
    error,
    artifacts,
    state: reconciledState,
    iterations: state.iterations,
  }));
  const record = {
    schema: DETERMINISTIC_LOOP_ITERATION_SCHEMA,
    loop_id: loopId,
    iteration: current.iteration,
    attempt: current.attempt,
    input: current.input,
    outcome,
    artifacts,
    state: reconciledState,
    stop,
  };
  const nextDurableState = checkpointState({
    ...state,
    state: reconciledState,
    current: null,
    iterations: [...state.iterations, record],
  }, 'completed', { iteration: current.iteration, attempt: current.attempt, artifacts });
  if (stop.stop) {
    return completeDurableState(nextDurableState, loopStatus(nextDurableState.iterations), stop.reason || 'stop_criteria_satisfied');
  }
  if (nextDurableState.iterations.length >= maxIterations) {
    return completeDurableState(nextDurableState, loopStatus(nextDurableState.iterations), 'max_iterations_reached');
  }
  return nextDurableState;
}

function normalizeDurableState(value, context = {}) {
  if (isPlainObject(value) && value.schema === DETERMINISTIC_LOOP_DURABLE_STATE_SCHEMA) {
    return {
      ...value,
      loop_id: value.loop_id || context.loopId || context.loop_id || 'deterministic-loop',
      state: clonePlainObject(value.state),
      iterations: Array.isArray(value.iterations) ? value.iterations : [],
      checkpoints: Array.isArray(value.checkpoints) ? value.checkpoints : [],
      current: isPlainObject(value.current) ? value.current : null,
      done: Boolean(value.done),
    };
  }
  const input = isPlainObject(value) ? value : {};
  return {
    schema: DETERMINISTIC_LOOP_DURABLE_STATE_SCHEMA,
    loop_id: context.loopId || context.loop_id || 'deterministic-loop',
    status: 'running',
    state: clonePlainObject(input.state || input.initialState || input.initial_state || (Object.keys(input).length > 0 ? input : context.initialState)),
    iterations: Array.isArray(input.iterations) ? input.iterations : [],
    checkpoints: Array.isArray(input.checkpoints) ? input.checkpoints : [],
    current: isPlainObject(input.current) ? input.current : null,
    done: Boolean(input.done),
  };
}

function checkpointState(state, type, data = {}) {
  const checkpoint = {
    schema: DETERMINISTIC_LOOP_CHECKPOINT_SCHEMA,
    loop_id: state.loop_id,
    type,
    sequence: state.checkpoints.length + 1,
    iteration: data.iteration || state.current?.iteration || 0,
    attempt: data.attempt || state.current?.attempt || 0,
    token: data.token || state.current?.token || '',
    artifacts: Array.isArray(data.artifacts) ? data.artifacts : [],
    data,
  };
  return { ...state, checkpoints: [...state.checkpoints, checkpoint] };
}

function completeDurableState(state, status, reason) {
  return checkpointState({ ...state, status, done: true, current: null, stop_reason: reason }, 'done', { reason });
}

function defaultBuildIteration({ state }) {
  return state.input || state.request || state;
}

function defaultReconcile({ state }) {
  return state;
}

function defaultShouldRetry() {
  return false;
}

function defaultStopCriteria() {
  return false;
}

function evaluateStopCriteria(criteria, context) {
  if (typeof criteria === 'function') {
    return criteria(context);
  }
  if (isPlainObject(criteria)) {
    if (criteria.path && Object.prototype.hasOwnProperty.call(criteria, 'equals')) {
      return getPath(context.state, criteria.path) === criteria.equals;
    }
    if (Array.isArray(criteria.outcome_status)) {
      return criteria.outcome_status.includes(context.outcome?.status);
    }
  }
  return defaultStopCriteria(context);
}

function normalizeStopDecision(value) {
  if (isPlainObject(value)) {
    return {
      stop: Boolean(value.stop),
      reason: value.reason || '',
      data: isPlainObject(value.data) ? value.data : undefined,
    };
  }
  return { stop: Boolean(value), reason: Boolean(value) ? 'stop_criteria_satisfied' : '' };
}

function normalizeArtifactRecords(value, context = {}) {
  const entries = Array.isArray(value) ? value : Object.entries(isPlainObject(value) ? value : {}).map(([name, artifact]) => ({ name, artifact }));
  return entries.map((entry, index) => {
    const artifact = isPlainObject(entry.artifact) ? entry.artifact : entry;
    return {
      schema: DETERMINISTIC_LOOP_ARTIFACT_SCHEMA,
      id: artifact.id || artifact.name || entry.name || `iteration-${context.iteration || 0}-artifact-${index + 1}`,
      loop_id: context.loopId || context.loop_id || '',
      iteration: context.iteration || 0,
      name: artifact.name || entry.name || artifact.id || '',
      path: artifact.path || '',
      url: artifact.url || '',
      sha256: artifact.sha256 || '',
      metadata: isPlainObject(artifact.metadata) ? artifact.metadata : {},
    };
  });
}

function loopStatus(iterations) {
  if (iterations.length === 0) {
    return 'empty';
  }
  const lastOutcome = iterations[iterations.length - 1]?.outcome || {};
  if (lastOutcome.status === 'failed') {
    return 'failed';
  }
  return 'completed';
}

function errorOutcome(error) {
  return {
    status: 'failed',
    summary: error && error.message ? error.message : String(error),
  };
}

function getPath(value, path) {
  return String(path).split('.').filter(Boolean).reduce((current, segment) => {
    if (!current || typeof current !== 'object') {
      return undefined;
    }
    return current[segment];
  }, value);
}

function clonePlainObject(value) {
  return isPlainObject(value) ? { ...value } : {};
}

function requiredFunction(value, name) {
  if (typeof value !== 'function') {
    throw new Error(`${name} must be a function.`);
  }
  return value;
}

function positiveInteger(value, fallback) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function nonNegativeInteger(value, fallback) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function isPlainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
  DETERMINISTIC_LOOP_ARTIFACT_SCHEMA,
  DETERMINISTIC_LOOP_CHECKPOINT_SCHEMA,
  DETERMINISTIC_LOOP_DURABLE_STATE_SCHEMA,
  DETERMINISTIC_LOOP_ITERATION_SCHEMA,
  DETERMINISTIC_LOOP_RESULT_SCHEMA,
  createDurableDeterministicLoop,
  runDeterministicLoop,
};
