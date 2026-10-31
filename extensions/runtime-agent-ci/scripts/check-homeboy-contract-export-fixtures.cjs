#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const DEFAULT_FIXTURE_PATH = path.join(
  __dirname,
  '..',
  'tests',
  'fixtures',
  'homeboy-contract-export.generated.json'
);

function checkHomeboyContractExportFixtures(options = {}) {
  const command = options.homeboyCommand || process.env.HOMEBOY_COMMAND || 'homeboy';
  const spawn = options.spawnSync || spawnSync;
  const fixturePath = options.fixturePath || DEFAULT_FIXTURE_PATH;
  const tempRoot = options.tempRoot || fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-contract-export-'));
  const env = { ...process.env, ...(options.env || {}) };
  const result = spawn(command, ['contract', 'export', '--dir', tempRoot], { encoding: 'utf8', env });
  const stdout = typeof result.stdout === 'string' ? result.stdout.trim() : '';
  const stderr = typeof result.stderr === 'string' ? result.stderr.trim() : '';

  if (result.error && result.error.code === 'ENOENT') {
    return skip(`homeboy contract export fixture check skipped: ${command} was not found`, result);
  }

  if (result.status !== 0) {
    if (/unrecognized|unknown|invalid/i.test(stderr)) {
      return skip('homeboy contract export fixture check skipped: contract export is unavailable', result);
    }
    return fail(`homeboy contract export --dir failed with status ${result.status}: ${stderr || stdout}`, result);
  }

  const fixture = readJson(fixturePath);
  const schemaCatalog = readJson(path.join(tempRoot, 'schema-catalog.json'));
  const errors = compareSchemaCatalogFixture(schemaCatalog, fixture.schema_catalog || {});

  if (errors.length > 0) {
    return fail(`homeboy contract export fixture check failed: ${errors.join('; ')}`, result);
  }

  return {
    status: 'passed',
    message: `homeboy contract export fixture check passed via ${command} contract export --dir`,
    tempRoot,
  };
}

function compareSchemaCatalogFixture(schemaCatalog, fixture) {
  const errors = [];
  if (schemaCatalog.schema !== fixture.schema) {
    errors.push(`schema catalog expected ${fixture.schema}, got ${schemaCatalog.schema || '(missing)'}`);
  }

  const contractsById = new Map((schemaCatalog.contracts || []).map((contract) => [contract.id, contract]));

  for (const expectedId of fixture.contract_ids || []) {
    const contract = contractsById.get(expectedId);
    if (!contract) {
      errors.push(`missing contract id ${expectedId}`);
      continue;
    }
    if (!contract.example || contract.example.schema !== expectedId) {
      errors.push(`${expectedId} example.schema expected ${expectedId}, got ${contract.example?.schema || '(missing)'}`);
    }
  }

  for (const [contractId, expectedExample] of Object.entries(fixture.examples || {})) {
    const actualExample = contractsById.get(contractId)?.example;
    if (!actualExample) {
      errors.push(`${contractId} example is missing`);
      continue;
    }
    compareSubset(actualExample, expectedExample, `examples.${contractId}`, errors);
  }

  return errors;
}

function compareSubset(actual, expected, label, errors) {
  if (expected && typeof expected === 'object' && !Array.isArray(expected)) {
    if (!actual || typeof actual !== 'object' || Array.isArray(actual)) {
      errors.push(`${label} expected object, got ${actual === undefined ? '(missing)' : typeof actual}`);
      return;
    }
    for (const [key, expectedValue] of Object.entries(expected)) {
      compareSubset(actual[key], expectedValue, `${label}.${key}`, errors);
    }
    return;
  }

  if (actual !== expected) {
    errors.push(`${label} expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function skip(message, result) {
  return { status: 'skipped', message, result };
}

function fail(message, result) {
  return { status: 'failed', message, result };
}

module.exports = {
  DEFAULT_FIXTURE_PATH,
  checkHomeboyContractExportFixtures,
  compareSchemaCatalogFixture,
};

if (require.main === module) {
  const result = checkHomeboyContractExportFixtures();
  process.stdout.write(`${result.message}\n`);
  if (result.status === 'failed') {
    process.exitCode = 1;
  }
}
