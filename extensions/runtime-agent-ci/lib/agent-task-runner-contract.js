'use strict';

const AGENT_TASK_RUNNER_SPEC_SCHEMA = 'homeboy/agent-task-runner-spec/v1';

function agentTaskRunnerSpec(options = {}) {
  const executorConfig = options.config || options.executorConfig || options.executor_config;
  if (!executorConfig || typeof executorConfig !== 'object' || Array.isArray(executorConfig)) {
    throw new Error('config is required.');
  }

  const backend = requiredString(options.backend, 'backend');
  const runtime = requiredString(options.runtime || options.runtimeId || options.runtime_id, 'runtime');
  const taskTimeoutSeconds = options.taskTimeoutSeconds || options.task_timeout_seconds;
  const limits = stripUndefined({
    ...(taskTimeoutSeconds ? { task_timeout_seconds: taskTimeoutSeconds } : {}),
    ...(options.limits || {}),
  });

  const spec = stripUndefined({
    schema: AGENT_TASK_RUNNER_SPEC_SCHEMA,
    executor: stripUndefined({
      backend,
      runtime,
      ...(normalizeArray(options.secretEnv || options.secret_env).length > 0
        ? { secret_env: normalizeArray(options.secretEnv || options.secret_env) }
        : {}),
      config: executorConfig,
    }),
    limits,
    expected_artifacts: normalizeArray(options.expectedArtifacts || options.expected_artifacts),
  });

  validateAgentTaskRunnerSpec(spec);
  return spec;
}

function agentTaskRequestFromRunnerSpec(options = {}) {
  const runnerSpec = validateAgentTaskRunnerSpec(options.runnerSpec || options.runner_spec);
  return stripUndefined({
    executor: runnerSpec.executor,
    limits: runnerSpec.limits,
    expected_artifacts: runnerSpec.expected_artifacts,
  });
}

function validateAgentTaskRunnerSpec(spec) {
  if (!spec || typeof spec !== 'object' || Array.isArray(spec)) {
    throw new Error('runner spec must be an object.');
  }
  if (spec.schema !== AGENT_TASK_RUNNER_SPEC_SCHEMA) {
    throw new Error(`runner spec schema must be ${AGENT_TASK_RUNNER_SPEC_SCHEMA}.`);
  }
  if (!spec.executor || typeof spec.executor !== 'object' || Array.isArray(spec.executor)) {
    throw new Error('runner spec executor is required.');
  }
  requiredString(spec.executor.backend, 'runner spec executor.backend');
  requiredString(spec.executor.runtime, 'runner spec executor.runtime');
  if (!spec.executor.config || typeof spec.executor.config !== 'object' || Array.isArray(spec.executor.config)) {
    throw new Error('runner spec executor.config is required.');
  }
  if (spec.executor.secret_env !== undefined && !Array.isArray(spec.executor.secret_env)) {
    throw new Error('runner spec executor.secret_env must be an array.');
  }
  if (spec.limits !== undefined && (typeof spec.limits !== 'object' || Array.isArray(spec.limits))) {
    throw new Error('runner spec limits must be an object.');
  }
  if (spec.expected_artifacts !== undefined && !Array.isArray(spec.expected_artifacts)) {
    throw new Error('runner spec expected_artifacts must be an array.');
  }
  return spec;
}

function normalizeArray(value) {
  return Array.isArray(value) ? value.filter(Boolean) : [];
}

function requiredString(value, name) {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`${name} is required.`);
  }
  return value;
}

function stripUndefined(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return value;
  }
  return Object.fromEntries(
    Object.entries(value).filter(([, entry]) => entry !== undefined)
  );
}

module.exports = {
  AGENT_TASK_RUNNER_SPEC_SCHEMA,
  agentTaskRequestFromRunnerSpec,
  agentTaskRunnerSpec,
  validateAgentTaskRunnerSpec,
};
