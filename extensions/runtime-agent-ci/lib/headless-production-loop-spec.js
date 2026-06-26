'use strict';

function materializeHeadlessProductionLoopSpec(spec = {}, options = {}) {
  const base = requiredObject(spec, 'spec');
  const overrides = runtimeOverrides(options);
  const revolutions = positiveInteger(options.revolutions || options.maxRevolutions || options.max_revolutions || options.iterations || options.maxIterations || options.max_iterations);
  const tasks = loopTasks(base).map((task) => materializeTask(task, { overrides, revolutions }));
  return cleanObject({
    ...base,
    ...overrides,
    ...(revolutions ? { loop_policy: materializeLoopPolicy(base.loop_policy, revolutions) } : {}),
    tasks,
  });
}

function materializeTask(task, { overrides = {}, revolutions = 0 } = {}) {
  return cleanObject({
    ...task,
    ...overrides,
    ...(revolutions ? { loop_policy: materializeLoopPolicy(task.loop_policy, revolutions) } : {}),
  });
}

function materializeLoopPolicy(policy = {}, revolutions = 0) {
  const normalized = optionalObject(policy);
  if (!revolutions) {
    return normalized;
  }
  return {
    ...normalized,
    mode: normalized.mode || 'count',
    max_iterations: revolutions,
    max_revolutions: revolutions,
  };
}

function runtimeOverrides(options = {}) {
  const provider = stringValue(options.provider);
  return cleanObject({
    runtime_id: stringValue(options.runtimeId || options.runtime_id || options.runtime),
    runtime_profile: stringValue(options.runtimeProfile || options.runtime_profile || options.profile),
    runtime_profiles: objectValue(options.runtimeProfiles || options.runtime_profiles),
    provider,
    model: stringValue(options.model),
    provider_plugin_paths: arrayValue(options.providerPluginPaths || options.provider_plugin_paths),
    provider_plugins: arrayValue(options.providerPlugins || options.provider_plugins),
    secret_env: secretEnvOverrides(options, provider),
    runtime_env: objectValue(options.runtimeEnv || options.runtime_env),
    runtime_config_mounts: arrayValue(options.runtimeConfigMounts || options.runtime_config_mounts),
    runtime_state_mounts: arrayValue(options.runtimeStateMounts || options.runtime_state_mounts),
  });
}

function secretEnvOverrides(options = {}, provider = '') {
  return arrayValue(options.secretEnv || options.secret_env) || providerDefaultSecretEnv(provider, options.runtime || options.runtime_manifest || options.runtimeManifest);
}

function providerDefaultSecretEnv(provider = '', runtime = {}) {
  const defaults = runtime?.executor?.provider_defaults?.[provider] || runtime?.provider_defaults?.[provider];
  if (!defaults || typeof defaults !== 'object' || Array.isArray(defaults)) {
    return undefined;
  }
  return uniqueStrings([
    ...(arrayValue(defaults.secret_env) || []),
    ...(arrayValue(defaults.required_secret_env) || []),
    ...(arrayValue(defaults.optional_secret_env) || []),
  ]);
}

function loopTasks(spec) {
  const tasks = Array.isArray(spec.tasks) && spec.tasks.length > 0 ? spec.tasks : [spec];
  return tasks.map((task, index) => {
    const normalized = requiredObject(task, `tasks[${index}]`);
    return {
      ...normalized,
      task_id: normalized.task_id || normalized.workload_id || `${loopId(spec)}-${index + 1}`,
      workload_id: normalized.workload_id || normalized.task_id || `${loopId(spec)}-${index + 1}`,
    };
  });
}

function loopId(spec) {
  return spec.loop_id || spec.plan_id || spec.workload_id || 'headless-production-loop';
}

function parseJsonObject(value, name) {
  if (!value) {
    return null;
  }
  const parsed = typeof value === 'string' ? JSON.parse(value) : value;
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error(`${name} must be a JSON object.`);
  }
  return parsed;
}

function parseJsonArray(value, name) {
  if (!value) {
    return [];
  }
  const parsed = typeof value === 'string' ? JSON.parse(value) : value;
  if (!Array.isArray(parsed)) {
    throw new Error(`${name} must be a JSON array.`);
  }
  return parsed;
}

function positiveInteger(value) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 0;
}

function stringValue(value) {
  return typeof value === 'string' && value.trim() !== '' ? value.trim() : undefined;
}

function objectValue(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

function arrayValue(value) {
  return Array.isArray(value) && value.length > 0 ? value : undefined;
}

function uniqueStrings(values) {
  return Array.from(new Set(values.filter((value) => typeof value === 'string' && value.length > 0))).sort();
}

function optionalObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function requiredObject(value, name) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${name} must be an object.`);
  }
  return value;
}

function cleanObject(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => {
    if (entry === undefined || entry === null || entry === '') {
      return false;
    }
    if (Array.isArray(entry)) {
      return entry.length > 0;
    }
    if (entry && typeof entry === 'object') {
      return Object.keys(entry).length > 0;
    }
    return true;
  }));
}

module.exports = {
  materializeHeadlessProductionLoopSpec,
  parseJsonArray,
  parseJsonObject,
  providerDefaultSecretEnv,
};
