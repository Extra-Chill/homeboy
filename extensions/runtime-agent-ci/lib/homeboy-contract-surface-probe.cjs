'use strict';

const { spawnSync } = require('node:child_process');

const {
  CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS,
  runtimeContractConstantsFromHomeboyOutput,
} = require('./runtime-contracts.cjs');

const CONTRACT_COMMANDS = Object.freeze([
  ['contract', 'constants', 'all'],
  ['contract', 'constants', 'artifact-manifest'],
  ['contract', 'constants', 'secret-env-plan'],
]);

const LOCAL_ARTIFACT_MANIFEST_CONSTANTS = CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS.artifact_manifest;
const CORE_PUBLISHED_CONTRACT_CONSTANTS = CORE_PUBLISHED_RUNTIME_CONTRACT_CONSTANTS;

function probeHomeboyContractSurface(options = {}) {
  const command = options.homeboyCommand || process.env.HOMEBOY_COMMAND || 'homeboy';
  const spawn = options.spawnSync || spawnSync;
  const env = { ...process.env, ...(options.env || {}) };
  const attempts = [];

  for (const argv of CONTRACT_COMMANDS) {
    const result = spawn(command, argv, { encoding: 'utf8', env });
    const stdout = typeof result.stdout === 'string' ? result.stdout.trim() : '';
    const stderr = typeof result.stderr === 'string' ? result.stderr.trim() : '';
    attempts.push({ argv, status: result.status, stdout, stderr, error: result.error ? result.error.message : '' });

    if (result.error && result.error.code === 'ENOENT') {
      return skip(`homeboy contract surface probe skipped: ${command} was not found`, attempts);
    }

    if (result.status !== 0 || !stdout) {
      continue;
    }

    let contract;
    try {
      contract = JSON.parse(stdout);
    } catch (error) {
      return fail(`homeboy contract surface probe failed: ${argv.join(' ')} did not emit JSON (${error.message})`, attempts);
    }

    const validation = validateRuntimeContractConstants({ contract, command, argv, attempts });
    if (validation.status === 'passed' || validation.status === 'failed') {
      return validation;
    }
  }

  return skip('homeboy contract surface probe skipped: no supported Homeboy contract command is available yet', attempts);
}

function validateRuntimeContractConstants({ contract, command, argv, attempts }) {
  const actualConstants = runtimeContractConstantsFromHomeboyOutput(contract);
  const expectedConstants = expectedConstantsForCommand(argv);
  const comparableConstants = comparableConstantsForProbe(actualConstants, expectedConstants, argv);
  if (Object.keys(actualConstants).length === 0 || Object.keys(comparableConstants).length === 0) {
    return skip(`homeboy contract surface probe skipped: ${argv.join(' ')} did not expose runtime-agent constants`, attempts);
  }

  const errors = [];

  for (const [contractName, expectedContract] of Object.entries(comparableConstants)) {
    const actualContract = actualConstants[contractName] || {};
    for (const [name, expected] of Object.entries(expectedContract)) {
      const actual = actualContract[name];
      if (typeof actual !== 'string' || actual.length === 0) {
        errors.push(`${contractName}.${name} is missing from Homeboy contract output`);
      } else if (actual !== expected) {
        errors.push(`${contractName}.${name} expected ${expected}, got ${actual}`);
      }
    }
  }

  if (errors.length > 0) {
    return fail(`homeboy contract surface probe failed: ${errors.join('; ')}`, attempts, { command, argv });
  }

  return {
    status: 'passed',
    message: `homeboy contract surface probe passed via ${command} ${argv.join(' ')}`,
    command,
    argv,
    constants: actualConstants,
    attempts,
  };
}

function comparableConstantsForProbe(actualConstants, expectedConstants, argv) {
  const contractId = argv[2] || '';
  return Object.fromEntries(
    Object.entries(expectedConstants).filter(([contractName]) => actualConstants[contractName] || contractId !== 'all')
  );
}

function expectedConstantsForCommand(argv) {
  const contractId = argv[2] || '';
  if (contractId === 'all') {
    return CORE_PUBLISHED_CONTRACT_CONSTANTS;
  }
  if (contractId === 'artifact-manifest') {
    return { artifact_manifest: CORE_PUBLISHED_CONTRACT_CONSTANTS.artifact_manifest };
  }
  if (contractId === 'secret-env-plan') {
    return { secret_env_plan: CORE_PUBLISHED_CONTRACT_CONSTANTS.secret_env_plan };
  }
  return {};
}

function skip(message, attempts) {
  return { status: 'skipped', message, attempts };
}

function fail(message, attempts, extra = {}) {
  return { status: 'failed', message, attempts, ...extra };
}

module.exports = {
  CONTRACT_COMMANDS,
  CORE_PUBLISHED_CONTRACT_CONSTANTS,
  LOCAL_ARTIFACT_MANIFEST_CONSTANTS,
  probeHomeboyContractSurface,
};
