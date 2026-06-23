#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { runGenericAgentLoop, writeGenericAgentLoopArtifacts } = require('../lib/generic-agent-loop-runner');
const { DEFAULT_RUNTIME_ID, resolveRuntimeProvider } = require('../lib/runtime-provider-resolver.cjs');

try {
  const configPath = process.argv[2] || process.env.HOMEBOY_RUNTIME_AGENT_CONFIG_PATH || '';
  if (!configPath) {
    throw new Error('Pass an agent loop JSON plan path as argv[1] or HOMEBOY_RUNTIME_AGENT_CONFIG_PATH.');
  }
  const plan = JSON.parse(fs.readFileSync(configPath, 'utf8'));
  const repoRoot = path.resolve(__dirname, '..', '..');
  const runtime = resolveRuntimeProvider(plan.runtime_id || process.env.RUNTIME || process.env.RUNTIME_PROVIDER || process.env.BACKEND || DEFAULT_RUNTIME_ID, {
    repoRoot,
    workspace: plan.component_path || process.cwd(),
    executor: plan.executor || {},
  });
  const result = runGenericAgentLoop({
    plan,
    runtime,
    configPath,
    repoRoot,
    extensionPath: plan.homeboy_extensions_path || repoRoot,
    replayBundleDir: plan.replay_bundle_dir || process.env.HOMEBOY_RUNTIME_AGENT_REPLAY_BUNDLE_DIR,
    validationPolicy: {
      scenario_id: plan.workload_id,
      success_requires_pr: plan.success_requires_pr,
      success_completion_outcomes: plan.success_completion_outcomes,
    },
  });
  writeGenericAgentLoopArtifacts({
    outcome: result.outcome,
    results: result.results,
    outcomeFile: process.env.HOMEBOY_AGENT_TASK_OUTCOME_FILE || '',
    resultsFile: process.env.HOMEBOY_RUNTIME_AGENT_RESULTS_FILE || '',
  });
  process.stdout.write(`${JSON.stringify(result.outcome, null, 2)}\n`);
  process.exitCode = result.outcome.status === 'succeeded' || result.outcome.status === 'no_op' ? 0 : 1;
} catch (error) {
  process.stderr.write(`${error && error.message ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
