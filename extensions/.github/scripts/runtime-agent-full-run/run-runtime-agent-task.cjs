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
const { resolveRuntimeProvider, runtimeIdFromOptions } = require('../../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');
const {
  genericAgentLoopStdoutSummary,
  runGenericAgentLoop,
  writeGenericAgentLoopArtifacts,
} = require('../../../runtime-agent-ci');
const { resolveControllerLoopProofPolicy } = require('./lib/proof-profile.cjs');

const SCRIPT_DIR = __dirname;
const REPO_ROOT = path.resolve(SCRIPT_DIR, '..', '..', '..');

function readConfigPath() {
  const configPath = process.argv[2] || process.env.HOMEBOY_RUNTIME_AGENT_CONFIG_PATH || '';
  if (!configPath) {
    throw new Error('Pass a runtime agent config JSON path as argv[1] or HOMEBOY_RUNTIME_AGENT_CONFIG_PATH.');
  }
  return configPath;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

try {
  const configPath = readConfigPath();
  const config = readJson(configPath);
  const controllerLoopProofPolicy = resolveControllerLoopProofPolicy(config);
  const runtime = resolveRuntimeProvider(runtimeIdFromOptions({ runtime_id: config.runtime_id || config.runtime }, process.env), { repoRoot: REPO_ROOT, workspace: config.component_path || process.cwd(), executor: config.executor || {} });
  const result = runGenericAgentLoop({
    plan: config,
    runtime,
    configPath,
    repoRoot: REPO_ROOT,
    extensionPath: REPO_ROOT,
    replayBundleDir: process.env.HOMEBOY_RUNTIME_AGENT_REPLAY_BUNDLE_DIR,
    validate: true,
    validationPolicy: {
      scenario_id: config.workload_id,
      success_requires_pr: config.success_requires_pr,
      success_completion_outcomes: config.success_completion_outcomes,
      required_evidence_refs: config.required_evidence_refs || config.required_evidence || [],
      controller_loop_proof: controllerLoopProofPolicy,
    },
  });
  writeGenericAgentLoopArtifacts({
    outcome: result.outcome,
    results: result.results,
    outcomeFile: process.env.HOMEBOY_AGENT_TASK_OUTCOME_FILE || '',
    resultsFile: process.env.HOMEBOY_RUNTIME_AGENT_RESULTS_FILE || '',
  });
  process.stdout.write(`${JSON.stringify(genericAgentLoopStdoutSummary({
    outcome: result.outcome,
    results: result.results,
    outcomeFile: process.env.HOMEBOY_AGENT_TASK_OUTCOME_FILE || '',
    resultsFile: process.env.HOMEBOY_RUNTIME_AGENT_RESULTS_FILE || '',
  }), null, 2)}\n`);
  process.exitCode = result.outcome.status === 'succeeded' || result.outcome.status === 'no_op' ? 0 : 1;
} catch (error) {
  console.error(error && error.message ? error.message : String(error));
  process.exitCode = 1;
}
