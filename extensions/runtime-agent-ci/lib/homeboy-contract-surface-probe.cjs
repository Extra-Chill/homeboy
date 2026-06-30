'use strict';

const { spawnSync } = require('node:child_process');

const {
  ARTIFACT_MANIFEST_FILE,
  ARTIFACT_MANIFEST_SCHEMA,
  ARTIFACT_PATHS_SCHEMA,
  RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
} = require('./runtime-contracts.cjs');

const CONTRACT_COMMANDS = Object.freeze([
  ['contract', 'constants', '--format=json'],
  ['contract', 'show', '--format=json'],
  ['contract', 'export', '--format=json'],
]);

const MIRRORED_ARTIFACT_MANIFEST_CONSTANTS = Object.freeze({
  ARTIFACT_MANIFEST_FILE,
  ARTIFACT_MANIFEST_SCHEMA,
  ARTIFACT_PATHS_SCHEMA,
  RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
});

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

    return validateArtifactManifestConstants({ contract, command, argv, attempts });
  }

  return skip('homeboy contract surface probe skipped: no supported Homeboy contract command is available yet', attempts);
}

function validateArtifactManifestConstants({ contract, command, argv, attempts }) {
  const errors = [];

  for (const [name, expected] of Object.entries(MIRRORED_ARTIFACT_MANIFEST_CONSTANTS)) {
    const actual = contractValue(contract, name);
    if (typeof actual !== 'string' || actual.length === 0) {
      errors.push(`${name} is missing from Homeboy contract output`);
    } else if (actual !== expected) {
      errors.push(`${name} expected ${expected}, got ${actual}`);
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
    constants: MIRRORED_ARTIFACT_MANIFEST_CONSTANTS,
    attempts,
  };
}

function contractValue(contract, name) {
  const candidates = valueCandidates(name);
  for (const path of candidates) {
    const value = readPath(contract, path);
    if (typeof value === 'string' && value.length > 0) {
      return value;
    }
  }
  return '';
}

function valueCandidates(name) {
  const camel = name.toLowerCase().replace(/_([a-z])/g, (_match, letter) => letter.toUpperCase());
  const lower = name.toLowerCase();

  const candidates = [
    ['constants', name],
    ['constants', lower],
    ['constants', camel],
    [name],
    [lower],
    [camel],
  ];

  if (name === 'ARTIFACT_MANIFEST_SCHEMA') {
    candidates.push(
      ['artifact_manifest', 'schema'],
      ['artifactManifest', 'schema'],
      ['schemas', 'artifact_manifest'],
      ['schemas', 'artifactManifest'],
      ['schemas', 'artifact', 'manifest'],
      ['contracts', 'artifact_manifest', 'schema']
    );
  } else if (name === 'ARTIFACT_MANIFEST_FILE') {
    candidates.push(
      ['artifact_manifest', 'file'],
      ['artifactManifest', 'file'],
      ['files', 'artifact_manifest'],
      ['files', 'artifactManifest'],
      ['contracts', 'artifact_manifest', 'file']
    );
  } else if (name === 'ARTIFACT_PATHS_SCHEMA') {
    candidates.push(
      ['artifact_paths', 'schema'],
      ['artifactPaths', 'schema'],
      ['schemas', 'artifact_paths'],
      ['schemas', 'artifactPaths'],
      ['contracts', 'artifact_paths', 'schema']
    );
  } else if (name === 'RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA') {
    candidates.push(
      ['runner_artifact_manifest_ref', 'schema'],
      ['runnerArtifactManifestRef', 'schema'],
      ['schemas', 'runner_artifact_manifest_ref'],
      ['schemas', 'runnerArtifactManifestRef'],
      ['contracts', 'runner_artifact_manifest_ref', 'schema']
    );
  }

  return candidates;
}

function readPath(value, path) {
  let current = value;
  for (const segment of path) {
    if (!current || typeof current !== 'object' || Array.isArray(current) || !Object.hasOwn(current, segment)) {
      return undefined;
    }
    current = current[segment];
  }
  return current;
}

function skip(message, attempts) {
  return { status: 'skipped', message, attempts };
}

function fail(message, attempts, extra = {}) {
  return { status: 'failed', message, attempts, ...extra };
}

module.exports = {
  CONTRACT_COMMANDS,
  MIRRORED_ARTIFACT_MANIFEST_CONSTANTS,
  probeHomeboyContractSurface,
};
