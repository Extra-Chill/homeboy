#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {
  runHeadlessDeterministicLoop,
  writeHeadlessDeterministicLoopArtifacts,
} = require('../lib/headless-deterministic-loop-runner');

try {
  const args = parseArgs(process.argv.slice(2));
  const specPath = args.spec || args.config || process.env.HOMEBOY_RUNTIME_AGENT_CONFIG_PATH || '';
  if (!specPath) {
    throw new Error('Pass --spec <path>, --config <path>, or HOMEBOY_RUNTIME_AGENT_CONFIG_PATH.');
  }
  const repoRoot = path.resolve(__dirname, '..', '..');
  const spec = JSON.parse(fs.readFileSync(specPath, 'utf8'));
  const result = runHeadlessDeterministicLoop({
    spec,
    configPath: specPath,
    repoRoot,
    extensionPath: spec.homeboy_extensions_path || repoRoot,
    replayBundleDir: process.env.HOMEBOY_RUNTIME_AGENT_REPLAY_BUNDLE_DIR,
    dryRun: args.dry_run === true || args['dry-run'] === true,
    validate: args.validate !== false,
  });
  writeHeadlessDeterministicLoopArtifacts({
    result,
    outcomeFile: args.outcome || process.env.HOMEBOY_AGENT_TASK_OUTCOME_FILE || '',
    resultsFile: args.results || process.env.HOMEBOY_RUNTIME_AGENT_RESULTS_FILE || '',
    eventsFile: args.events || process.env.HOMEBOY_RUNTIME_AGENT_EVENTS_FILE || '',
    loopResultFile: args.result || process.env.HOMEBOY_RUNTIME_AGENT_LOOP_RESULT_FILE || '',
  });
  process.stdout.write(`${JSON.stringify(result.outcome || result, null, 2)}\n`);
  process.exitCode = result.status === 'succeeded' ? 0 : 1;
} catch (error) {
  process.stderr.write(`${error && error.message ? error.message : String(error)}\n`);
  process.exitCode = 1;
}

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith('--')) {
      continue;
    }
    const key = arg.slice(2).replace(/-/g, '_');
    if (key === 'dry_run') {
      args[key] = true;
      continue;
    }
    if (key === 'no_validate') {
      args.validate = false;
      continue;
    }
    args[key] = argv[index + 1];
    index += 1;
  }
  return args;
}
