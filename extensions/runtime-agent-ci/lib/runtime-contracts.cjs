'use strict';

const CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS = Object.freeze({
  artifact_manifest: Object.freeze({
    file_name: 'homeboy-artifact-manifest.json',
    schema_id: 'homeboy/artifact-manifest/v1',
  }),
  secret_env_plan: Object.freeze({
    schema_id: 'homeboy/secret-env-plan/v1',
  }),
  run_location_index: Object.freeze({
    schema_id: 'homeboy/run-location-index/v1',
  }),
});

const LOCAL_RUN_OUTCOME_ENVELOPE_CONTRACT_CONSTANTS = validatedLocalSchemaFallback(
  'run_outcome_envelope',
  CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS.run_outcome_envelope,
  Object.freeze({ schema_id: 'homeboy/run-outcome-envelope/v1' })
);

// Extension-local artifact schemas. These remain here until Homeboy core exports
// contract constants for the runtime artifact path/ref boundary.
const EXTENSION_RUNTIME_CONTRACT_CONSTANTS = Object.freeze({
  artifact_paths: Object.freeze({
    schema_id: 'homeboy/runtime-agent-artifact-paths/v1',
  }),
  runner_artifact_manifest_ref: Object.freeze({
    schema_id: 'homeboy/runner-artifact-manifest-ref/v1',
  }),
});

const CORE_RUNTIME_CONTRACT_EXPORT_BLOCKERS = Object.freeze(Object.keys({
  ...(CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS.run_outcome_envelope ? {} : { run_outcome_envelope: LOCAL_RUN_OUTCOME_ENVELOPE_CONTRACT_CONSTANTS }),
  ...EXTENSION_RUNTIME_CONTRACT_CONSTANTS,
}));

const RUNTIME_CONTRACT_CONSTANTS = Object.freeze({
  ...CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS,
  run_outcome_envelope: CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS.run_outcome_envelope || LOCAL_RUN_OUTCOME_ENVELOPE_CONTRACT_CONSTANTS,
  ...EXTENSION_RUNTIME_CONTRACT_CONSTANTS,
});

const ARTIFACT_MANIFEST_CONTRACT_CONSTANTS = CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS.artifact_manifest;
const ARTIFACT_MANIFEST_SCHEMA = ARTIFACT_MANIFEST_CONTRACT_CONSTANTS.schema_id;
const ARTIFACT_MANIFEST_FILE = ARTIFACT_MANIFEST_CONTRACT_CONSTANTS.file_name;
const SECRET_ENV_PLAN_SCHEMA = CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS.secret_env_plan.schema_id;
const RUN_LOCATION_INDEX_SCHEMA = CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS.run_location_index.schema_id;
const RUN_OUTCOME_ENVELOPE_SCHEMA = RUNTIME_CONTRACT_CONSTANTS.run_outcome_envelope.schema_id;
const ARTIFACT_PATHS_SCHEMA = EXTENSION_RUNTIME_CONTRACT_CONSTANTS.artifact_paths.schema_id;
const RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA = EXTENSION_RUNTIME_CONTRACT_CONSTANTS.runner_artifact_manifest_ref.schema_id;
const CANONICAL_RUN_ARTIFACT_FILES = Object.freeze({
  events: 'events.json',
  status: 'status.json',
  results: 'results.json',
  outcome: 'outcome.json',
  run_outcome_envelope: 'run-outcome-envelope.json',
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
      allowed_env_names: uniqueStrings([
        'HOMEBOY_AGENT_RUNTIME_SECRET_ENV',
        ...Object.keys(secretEnvFallbacks || {}),
        ...Object.values(secretEnvFallbacks || {}).flat(),
      ]),
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

function validatedLocalSchemaFallback(contractName, coreConstants, fallback) {
  if (coreConstants && typeof coreConstants.schema_id === 'string' && coreConstants.schema_id.length > 0) {
    return coreConstants;
  }
  if (!fallback || typeof fallback.schema_id !== 'string' || fallback.schema_id.length === 0) {
    throw new Error(`${contractName}.schema_id fallback must be a non-empty string`);
  }
  return fallback;
}

function runtimeContractConstantsFromHomeboyOutput(output) {
  const constants = contractConstantsPayload(output);
  if (!constants) {
    return {};
  }

  const normalized = {};
  copyContractConstants(normalized, 'artifact_manifest', constants.artifact_manifest || constants.artifactManifest || constants, ['file_name', 'schema_id']);
  copyContractConstants(normalized, 'secret_env_plan', constants.secret_env_plan || constants.secretEnvPlan || constants, ['schema_id']);
  copyContractConstants(normalized, 'run_location_index', constants.run_location_index || constants.runLocationIndex, ['schema_id']);
  copyContractConstants(normalized, 'artifact_paths', constants.artifact_paths || constants.artifactPaths, ['schema_id']);
  copyContractConstants(normalized, 'runner_artifact_manifest_ref', constants.runner_artifact_manifest_ref || constants.runnerArtifactManifestRef, ['schema_id']);
  copyContractConstants(normalized, 'runner_execution_record', constants.runner_execution_record || constants.runnerExecutionRecord, ['schema_id']);
  copyContractConstants(normalized, 'path_materialization_plan', constants.path_materialization_plan || constants.pathMaterializationPlan, ['schema_id']);
  copyContractConstants(normalized, 'run_outcome_envelope', constants.run_outcome_envelope || constants.runOutcomeEnvelope, ['schema_id']);
  return normalized;
}

function contractConstantsPayload(output) {
  if (!output || typeof output !== 'object' || Array.isArray(output)) {
    return null;
  }
  const data = output.data && typeof output.data === 'object' && !Array.isArray(output.data) ? output.data : output;
  return data.constants && typeof data.constants === 'object' && !Array.isArray(data.constants) ? data.constants : null;
}

function copyContractConstants(target, contractName, source, names) {
  if (!source || typeof source !== 'object' || Array.isArray(source)) {
    return;
  }
  const values = {};
  for (const name of names) {
    const value = source[name];
    if (typeof value === 'string' && value.length > 0) {
      values[name] = value;
    }
  }
  if (Object.keys(values).length > 0) {
    target[contractName] = values;
  }
}

module.exports = {
  ARTIFACT_MANIFEST_CONTRACT_CONSTANTS,
  ARTIFACT_MANIFEST_FILE,
  ARTIFACT_MANIFEST_SCHEMA,
  ARTIFACT_PATHS_SCHEMA,
  CANONICAL_RUN_ARTIFACT_FILES,
  CORE_RUNTIME_CONTRACT_EXPORT_BLOCKERS,
  CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS,
  EXTENSION_RUNTIME_CONTRACT_CONSTANTS,
  RUN_LOCATION_INDEX_SCHEMA,
  RUN_OUTCOME_ENVELOPE_SCHEMA,
  RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
  RUNTIME_CONTRACT_CONSTANTS,
  SECRET_ENV_PLAN_SCHEMA,
  buildSecretEnvFallbacks,
  buildSecretEnvPlan,
  runtimeContractConstantsFromHomeboyOutput,
};
