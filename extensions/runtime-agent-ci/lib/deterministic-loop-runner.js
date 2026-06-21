'use strict';

const DETERMINISTIC_LOOP_RESULT_SCHEMA = 'homeboy/deterministic-loop-result/v1';
const DETERMINISTIC_LOOP_ITERATION_SCHEMA = 'homeboy/deterministic-loop-iteration/v1';
const DETERMINISTIC_LOOP_ARTIFACT_SCHEMA = 'homeboy/deterministic-loop-artifact/v1';

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

function isPlainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
  DETERMINISTIC_LOOP_ARTIFACT_SCHEMA,
  DETERMINISTIC_LOOP_ITERATION_SCHEMA,
  DETERMINISTIC_LOOP_RESULT_SCHEMA,
  runDeterministicLoop,
};
