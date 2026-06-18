#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
  createGenericFanoutReconcilePlan,
  createGenericFanoutReconcileResult,
} = require('../../lib/generic-fanout-reconcile-workflow');

function argValue(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function hasFlag(name) {
  return process.argv.includes(name);
}

function usage() {
  console.error('Usage: homeboy-generic-fanout-reconcile.cjs --config <config.json> [--items <items.json>] [--records <records.json>] [--plan <plan.json>] [--output <plan-or-result.json>]');
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
  const configPath = argValue('--config');
  if (!configPath || hasFlag('--help')) {
    usage();
  }

  const config = readJson(configPath, {});
  const items = readJson(argValue('--items'), config.items || []);
  const planPath = argValue('--plan');
  const recordsPath = argValue('--records');
  const outputPath = argValue('--output');

  if (recordsPath) {
    const plan = readJson(planPath, null) || createGenericFanoutReconcilePlan({ config, items });
    const result = await createGenericFanoutReconcileResult({
      config,
      plan,
      records: readJson(recordsPath, []),
    });
    writeJson(outputPath, result);
    return;
  }

  const plan = createGenericFanoutReconcilePlan({ config, items });
  writeJson(outputPath || planPath, plan);
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
