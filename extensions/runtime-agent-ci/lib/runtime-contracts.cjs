'use strict';

const SECRET_ENV_PLAN_SCHEMA = 'homeboy/secret-env-plan/v1';
const ARTIFACT_PATHS_SCHEMA = 'homeboy/runtime-agent-artifact-paths/v1';
const ARTIFACT_MANIFEST_SCHEMA = 'homeboy/artifact-manifest/v1';
const ARTIFACT_MANIFEST_FILE = 'homeboy-artifact-manifest.json';
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
function buildSecretEnvPlan({ secretEnv = [], runtimeEnv = {}, providerSecretEnvMapping = {}, secretEnvFallbacks = {} } = {}) {
  return {
    schema: SECRET_ENV_PLAN_SCHEMA,
    public_env: Object.fromEntries(Object.entries(runtimeEnv).filter(([, value]) => typeof value === 'string')),
    secret_env_names: uniqueStrings(secretEnv),
    requirements: uniqueStrings(secretEnv).map((name) => ({ name, required: true })),
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
  return fallbacks;
}

function uniqueStrings(values) {
  return Array.from(new Set(values.filter((value) => typeof value === 'string' && value.length > 0))).sort();
}

module.exports = {
  ARTIFACT_MANIFEST_FILE,
  ARTIFACT_MANIFEST_SCHEMA,
  ARTIFACT_PATHS_SCHEMA,
  CANONICAL_RUN_ARTIFACT_FILES,
  RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
  SECRET_ENV_PLAN_SCHEMA,
  buildSecretEnvFallbacks,
  buildSecretEnvPlan,
};
