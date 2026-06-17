#!/usr/bin/env node
'use strict';

const { findScenario, getByPath, parseJsonInput, readJsonFile, writeGithubOutput } = require('./lib/common.cjs');

function main() {
  const outputs = parseJsonInput('engine_data_outputs', process.env.ENGINE_DATA_OUTPUTS || '{}', 'object', {});
  if (Object.keys(outputs).length === 0) {
    writeGithubOutput({ engine_data_json: '{}' });
    return;
  }
  const scenario = findScenario(readJsonFile(process.env.RESULTS_FILE), process.env.FLOW_SLUG);
  const projected = {};
  for (const [key, expression] of Object.entries(outputs)) {
    projected[key] = getByPath(scenario, expression) ?? null;
  }
  writeGithubOutput({ engine_data_json: JSON.stringify(projected) });
}

try {
  main();
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
}
