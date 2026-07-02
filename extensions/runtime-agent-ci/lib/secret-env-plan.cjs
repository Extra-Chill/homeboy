'use strict';

const SECRET_ENV_PLAN_SCHEMA = 'homeboy/secret-env-plan/v1';

function buildSecretEnvPlan({ secretEnv = [], runtimeEnv = {}, providerSecretEnvMapping = {}, secretEnvFallbacks = {}, basePlan = {} } = {}) {
  const baseRequirements = Array.isArray(basePlan.requirements) ? basePlan.requirements.filter((entry) => entry && typeof entry === 'object' && !Array.isArray(entry)) : [];
  const secretEnvNames = uniqueStrings([
    ...normalizeStringArray(basePlan.secret_env_names),
    ...baseRequirements.map((entry) => entry.name),
    ...secretEnv,
  ]);
  const requirementNames = new Set(baseRequirements.map((entry) => entry.name).filter((name) => typeof name === 'string' && name.length > 0));
  return {
    ...basePlan,
    schema: SECRET_ENV_PLAN_SCHEMA,
    public_env: {
      ...(basePlan.public_env && typeof basePlan.public_env === 'object' && !Array.isArray(basePlan.public_env) ? basePlan.public_env : {}),
      ...Object.fromEntries(Object.entries(runtimeEnv).filter(([, value]) => typeof value === 'string')),
    },
    secret_env_names: secretEnvNames,
    requirements: [
      ...baseRequirements,
      ...secretEnvNames.filter((name) => !requirementNames.has(name)).map((name) => ({ name, required: true })),
    ],
    env_name_mapping: Object.fromEntries(Object.entries({
      provider_secret_env: Object.values(providerSecretEnvMapping).filter((value) => typeof value === 'string' && value.length > 0),
      secret_env_fallbacks: Object.values(secretEnvFallbacks).flat().filter((value) => typeof value === 'string' && value.length > 0),
    }).filter(([, names]) => names.length > 0)),
    inheritance: {
      require_declaration: true,
      allowed_env_names: uniqueStrings([
        'HOMEBOY_AGENT_RUNTIME_SECRET_ENV',
        ...Object.keys(secretEnvFallbacks || {}),
        ...Object.values(secretEnvFallbacks || {}).flat(),
      ]),
    },
  };
}

function buildSecretEnvFallbacks({
  githubTokenEnv,
  githubRepositoryTokenEnv,
  providerCanonicalSecretEnvNames = [],
  providerCredentialSourceEnvNames = [],
  secretEnvMap = {},
} = {}) {
  const fallbacks = {};
  if (githubTokenEnv && githubRepositoryTokenEnv && githubTokenEnv !== githubRepositoryTokenEnv) {
    fallbacks[githubTokenEnv] = [githubRepositoryTokenEnv];
  }
  if (providerCredentialSourceEnvNames.length > 0) {
    for (const canonical of providerCanonicalSecretEnvNames) {
      const sources = providerCredentialSourceEnvNames.filter((name) => name !== canonical);
      if (sources.length > 0) {
        fallbacks[canonical] = uniqueStrings([...(fallbacks[canonical] || []), ...sources]);
      }
    }
  }
  for (const [target, sources] of Object.entries(secretEnvMap || {})) {
    const sourceList = Array.isArray(sources) ? sources : [sources];
    const normalizedSources = sourceList.filter((name) => typeof name === 'string' && name.length > 0 && name !== target);
    if (typeof target === 'string' && target.length > 0 && normalizedSources.length > 0) {
      fallbacks[target] = uniqueStrings([...(fallbacks[target] || []), ...normalizedSources]);
    }
  }
  return fallbacks;
}

function normalizeSecretEnvInput(value) {
  if (typeof value !== 'string' || value.trim() === '') {
    return [];
  }
  const trimmed = value.trim();
  const entries = trimmed.startsWith('[') ? JSON.parse(trimmed) : trimmed.split(',').map((entry) => entry.trim()).filter(Boolean);
  if (!Array.isArray(entries)) {
    throw new Error('secret_env must be a JSON array or comma-separated list');
  }
  return entries.map((entry) => validateEnvName(entry, 'secret_env'));
}

function normalizeSecretEnvMap(map) {
  const normalized = {};
  for (const [target, sources] of Object.entries(map || {})) {
    validateEnvName(target, 'secret_env_map target');
    const sourceList = Array.isArray(sources) ? sources : [sources];
    if (sourceList.length === 0) {
      throw new Error(`secret_env_map.${target} requires at least one source env name`);
    }
    normalized[target] = sourceList.map((source) => validateEnvName(source, `secret_env_map.${target}`));
  }
  return normalized;
}

function secretEnvMapSourceNames(secretEnvMap) {
  const names = new Set();
  for (const [target, sources] of Object.entries(normalizeSecretEnvMap(secretEnvMap))) {
    validateEnvName(target, 'secret_env_map target');
    for (const source of sources) {
      names.add(source);
    }
  }
  return Array.from(names).sort();
}

function secretEnvNamesFromRequirements(requirements) {
  if (!Array.isArray(requirements)) {
    return [];
  }
  return requirements.map((entry) => entry?.name).filter((name) => typeof name === 'string' && name.length > 0);
}

function normalizeStringArray(value) {
  if (typeof value === 'string' && value.length > 0) {
    return [value];
  }
  return Array.isArray(value) ? value.filter((entry) => typeof entry === 'string' && entry.length > 0) : [];
}

function validateEnvName(name, label) {
  if (typeof name !== 'string' || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
    throw new Error(`${label} entries must be valid environment variable names`);
  }
  return name;
}

function uniqueStrings(values) {
  return Array.from(new Set(values.filter((value) => typeof value === 'string' && value.length > 0))).sort();
}

module.exports = {
  SECRET_ENV_PLAN_SCHEMA,
  buildSecretEnvFallbacks,
  buildSecretEnvPlan,
  normalizeSecretEnvInput,
  normalizeSecretEnvMap,
  secretEnvMapSourceNames,
  secretEnvNamesFromRequirements,
  validateEnvName,
};
