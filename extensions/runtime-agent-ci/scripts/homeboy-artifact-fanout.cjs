#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

const fs = require('node:fs');
const path = require('node:path');

const { runArtifactFanout } = require('../lib/artifact-fanout-materializer');

function argValue(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function hasFlag(name) {
  return process.argv.includes(name);
}

function usage() {
  console.error('Usage: homeboy-artifact-fanout.cjs --config <config.json> [--controller-input <input.json>] [--items <items.json>] [--mode plan|submit|run] [--output <result.json>]');
  process.exit(1);
}

function readJson(filePath, fallback) {
  if (!filePath) {
    return fallback;
  }
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function writeJson(filePath, value) {
  if (!filePath) {
    console.log(JSON.stringify(value, null, 2));
    return;
  }
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

async function main() {
  if (hasFlag('--help')) {
    usage();
  }
  const configPath = argValue('--config');
  const inlineConfig = argValue('--config-json');
  if (!configPath && !inlineConfig) {
    usage();
  }
  const config = inlineConfig ? JSON.parse(inlineConfig) : readJson(configPath, {});
  const controllerInput = readJson(argValue('--controller-input') || process.env.HOMEBOY_LOOP_ACTION_INPUT, null);
  const itemsPath = argValue('--items');
  const result = runArtifactFanout({
    config,
    controller_input: controllerInput,
    items: itemsPath ? readJson(itemsPath, []) : undefined,
    mode: argValue('--mode') || config.mode || 'plan',
    batch_id: argValue('--batch-id') || config.batch_id,
    homeboy_bin: argValue('--homeboy-bin') || config.homeboy_bin,
    max_drains: argValue('--max-drains') || config.max_drains,
  });
  const outputPath = argValue('--output');
  writeJson(outputPath, result);
  if (process.env.HOMEBOY_LOOP_ACTION_OUTPUT) {
    writeJson(process.env.HOMEBOY_LOOP_ACTION_OUTPUT, {
      schema: 'homeboy-extensions/artifact-fanout-command-output/v1',
      status: result.status,
      artifacts: {
        [config.output_artifact || config.outputArtifact || 'artifact_fanout_batch']: result,
      },
    });
  }
  process.exitCode = ['failed'].includes(result.status) ? 1 : 0;
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
