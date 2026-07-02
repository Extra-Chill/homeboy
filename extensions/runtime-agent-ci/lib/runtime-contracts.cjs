'use strict';

const SECRET_ENV_PLAN_SCHEMA = 'homeboy/secret-env-plan/v1';
const ARTIFACT_PATHS_SCHEMA = 'homeboy/runtime-agent-artifact-paths/v1';
const ARTIFACT_MANIFEST_CONTRACT_CONSTANTS = Object.freeze({
  file_name: 'homeboy-artifact-manifest.json',
  schema_id: 'homeboy/artifact-manifest/v1',
});
const ARTIFACT_MANIFEST_SCHEMA = ARTIFACT_MANIFEST_CONTRACT_CONSTANTS.schema_id;
const ARTIFACT_MANIFEST_FILE = ARTIFACT_MANIFEST_CONTRACT_CONSTANTS.file_name;
const RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA = 'homeboy/runner-artifact-manifest-ref/v1';
const CANONICAL_RUN_ARTIFACT_FILES = Object.freeze({
  events: 'events.json',
  status: 'status.json',
  results: 'results.json',
  outcome: 'outcome.json',
  fanout_run: 'fanout-run.json',
  loop_result: 'loop-result.json',
  loop_policy: 'loop-policy.json',
});

// Local adapter seam for the secret materialization contract. Keep the emitted
// shape stable until Homeboy core owns this schema and assembly.
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
      allowed_env_names: ['HOMEBOY_AGENT_RUNTIME_SECRET_ENV'],
    },
  };
}

// Build the declarative secret-env fallback map consumed by the run step. Each
// entry maps a target secret env name to an ordered list of source env names;
// the run step sets target = first non-empty source when target is unset.
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

function normalizeStringArray(value) {
  if (typeof value === 'string' && value.length > 0) {
    return [value];
  }
  return Array.isArray(value) ? value.filter((entry) => typeof entry === 'string' && entry.length > 0) : [];
}

function uniqueStrings(values) {
  return Array.from(new Set(values.filter((value) => typeof value === 'string' && value.length > 0))).sort();
}

module.exports = {
  ARTIFACT_MANIFEST_CONTRACT_CONSTANTS,
  ARTIFACT_MANIFEST_FILE,
  ARTIFACT_MANIFEST_SCHEMA,
  ARTIFACT_PATHS_SCHEMA,
  CANONICAL_RUN_ARTIFACT_FILES,
  RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
  SECRET_ENV_PLAN_SCHEMA,
  buildSecretEnvFallbacks,
  buildSecretEnvPlan,
};
