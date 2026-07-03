'use strict';

const fs = require('node:fs');
const { spawnSync } = require('node:child_process');

const REQUIRED_RUNTIME_CONTRACT_FIELDS = Object.freeze({
  artifact_manifest: Object.freeze(['file_name', 'schema_id']),
  secret_env_plan: Object.freeze(['schema_id']),
  run_location_index: Object.freeze(['schema_id']),
  artifact_paths: Object.freeze(['schema_id']),
  runner_artifact_manifest_ref: Object.freeze(['schema_id']),
  runner_execution_record: Object.freeze(['schema_id']),
  run_outcome_envelope: Object.freeze(['schema_id']),
});

const RUNTIME_CONTRACT_CONSTANTS = loadHomeboyRuntimeContractConstants();
const ARTIFACT_MANIFEST_CONTRACT_CONSTANTS = RUNTIME_CONTRACT_CONSTANTS.artifact_manifest;
const ARTIFACT_MANIFEST_SCHEMA = ARTIFACT_MANIFEST_CONTRACT_CONSTANTS.schema_id;
const ARTIFACT_MANIFEST_FILE = ARTIFACT_MANIFEST_CONTRACT_CONSTANTS.file_name;
const SECRET_ENV_PLAN_SCHEMA = RUNTIME_CONTRACT_CONSTANTS.secret_env_plan.schema_id;
const RUN_LOCATION_INDEX_SCHEMA = RUNTIME_CONTRACT_CONSTANTS.run_location_index.schema_id;
const RUN_OUTCOME_ENVELOPE_SCHEMA = RUNTIME_CONTRACT_CONSTANTS.run_outcome_envelope.schema_id;
const RUNNER_EXECUTION_RECORD_SCHEMA = RUNTIME_CONTRACT_CONSTANTS.runner_execution_record.schema_id;
const ARTIFACT_PATHS_SCHEMA = RUNTIME_CONTRACT_CONSTANTS.artifact_paths.schema_id;
const RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA = RUNTIME_CONTRACT_CONSTANTS.runner_artifact_manifest_ref.schema_id;
const {
  buildSecretEnvFallbacks,
  buildSecretEnvPlan,
} = require('./secret-env-plan.cjs');
const CANONICAL_RUN_ARTIFACT_FILES = Object.freeze({
  events: 'events.json',
  status: 'status.json',
  results: 'results.json',
  outcome: 'outcome.json',
  run_outcome_envelope: 'run-outcome-envelope.json',
  runner_execution_record: 'runner-execution-record.json',
  fanout_run: 'fanout-run.json',
  loop_result: 'loop-result.json',
  loop_policy: 'loop-policy.json',
});

function loadHomeboyRuntimeContractConstants(options = {}) {
  const output = loadHomeboyContractConstantsOutput(options);
  const constants = runtimeContractConstantsFromHomeboyOutput(output);
  validateRequiredRuntimeContractConstants(constants);
  return deepFreeze(constants);
}

function loadHomeboyContractConstantsOutput(options = {}) {
  const fixturePath = options.fixturePath || process.env.HOMEBOY_RUNTIME_CONTRACT_CONSTANTS_FIXTURE || '';
  if (fixturePath) {
    return JSON.parse(fs.readFileSync(fixturePath, 'utf8'));
  }

  const command = options.homeboyCommand || process.env.HOMEBOY_COMMAND || 'homeboy';
  const spawn = options.spawnSync || spawnSync;
  const env = { ...process.env, ...(options.env || {}) };
  const result = spawn(command, ['contract', 'constants', 'all'], { encoding: 'utf8', env });
  const stdout = typeof result.stdout === 'string' ? result.stdout.trim() : '';
  const stderr = typeof result.stderr === 'string' ? result.stderr.trim() : '';

  if (result.error) {
    throw new Error(`Homeboy runtime contract constants unavailable: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(`Homeboy runtime contract constants unavailable: ${command} contract constants all exited ${result.status}: ${stderr || stdout}`);
  }
  if (!stdout) {
    throw new Error(`Homeboy runtime contract constants unavailable: ${command} contract constants all emitted no JSON`);
  }

  try {
    return JSON.parse(stdout);
  } catch (error) {
    throw new Error(`Homeboy runtime contract constants unavailable: ${command} contract constants all emitted invalid JSON: ${error.message}`);
  }
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

function validateRequiredRuntimeContractConstants(constants) {
  const errors = [];
  for (const [contractName, fields] of Object.entries(REQUIRED_RUNTIME_CONTRACT_FIELDS)) {
    const contract = constants[contractName];
    if (!contract || typeof contract !== 'object' || Array.isArray(contract)) {
      errors.push(`${contractName} is missing from Homeboy contract constants`);
      continue;
    }
    for (const field of fields) {
      if (typeof contract[field] !== 'string' || contract[field].length === 0) {
        errors.push(`${contractName}.${field} is missing from Homeboy contract constants`);
      }
    }
  }
  if (errors.length > 0) {
    throw new Error(`Homeboy runtime contract constants are incomplete: ${errors.join('; ')}`);
  }
}

function deepFreeze(value) {
  if (!value || typeof value !== 'object' || Object.isFrozen(value)) {
    return value;
  }
  Object.freeze(value);
  for (const child of Object.values(value)) {
    deepFreeze(child);
  }
  return value;
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
  REQUIRED_RUNTIME_CONTRACT_FIELDS,
  RUN_LOCATION_INDEX_SCHEMA,
  RUN_OUTCOME_ENVELOPE_SCHEMA,
  RUNNER_EXECUTION_RECORD_SCHEMA,
  RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
  RUNTIME_CONTRACT_CONSTANTS,
  SECRET_ENV_PLAN_SCHEMA,
  buildSecretEnvFallbacks,
  buildSecretEnvPlan,
  loadHomeboyRuntimeContractConstants,
  runtimeContractConstantsFromHomeboyOutput,
  validateRequiredRuntimeContractConstants,
};
