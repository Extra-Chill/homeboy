#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

const fs = require('node:fs');
const path = require('node:path');
const {
  recordLifecycle,
  runDeterministicWorkspaceLifecycle,
  scenarioById,
} = require('../../../runtime-agent-ci/lib/workspace-publication-lifecycle.cjs');

function argValue(name, fallback = '') {
  const index = process.argv.indexOf(name);
  return index >= 0 && process.argv[index + 1] ? process.argv[index + 1] : fallback;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function main() {
  const resultsPath = argValue('--results');
  const configPath = argValue('--config');
  const scenarioId = argValue('--scenario');
  const workspace = path.resolve(argValue('--workspace', process.cwd()));
  if (!resultsPath || !configPath) {
    throw new Error('Usage: run-host-runner-lifecycle.cjs --results <path> --config <path> --scenario <id> [--workspace <path>]');
  }

  const results = readJson(resultsPath);
  const config = readJson(configPath);
  const scenario = scenarioById(results, scenarioId);
  if (!scenario) {
    throw new Error(`Scenario not found in results: ${scenarioId || '(first scenario)'}`);
  }

  const lifecycle = runDeterministicWorkspaceLifecycle(config, results, scenario, workspace);
  recordLifecycle(results, scenario, lifecycle);
  writeJson(resultsPath, results);
  if (!lifecycle.success) {
    throw new Error(lifecycle.error);
  }
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}

module.exports = { main };
