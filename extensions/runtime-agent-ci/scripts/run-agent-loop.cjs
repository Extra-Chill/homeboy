#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { runGenericAgentLoop, writeGenericAgentLoopArtifacts } = require('../lib/generic-agent-loop-runner');
const { runHeadlessDeterministicLoop, writeHeadlessDeterministicLoopArtifacts } = require('../lib/headless-deterministic-loop-runner');
const { resolveRuntimeProvider, runtimeIdFromOptions } = require('../lib/runtime-provider-resolver.cjs');

(async function main() {
  const configPath = process.argv[2] || process.env.HOMEBOY_RUNTIME_AGENT_CONFIG_PATH || '';
  if (!configPath) {
    throw new Error('Pass an agent loop JSON plan path as argv[1] or HOMEBOY_RUNTIME_AGENT_CONFIG_PATH.');
  }
  const plan = JSON.parse(fs.readFileSync(configPath, 'utf8'));
  const repoRoot = path.resolve(__dirname, '..', '..');
  const runtime = resolveRuntimeProvider(runtimeIdFromOptions({ runtime_id: plan.runtime_id || plan.runtime }, process.env), {
    repoRoot,
    workspace: plan.component_path || process.cwd(),
    executor: plan.executor || {},
  });
  if (plan.controller_execution || plan.controllerExecution || plan.controller) {
    const result = await runHeadlessDeterministicLoop({
      spec: plan,
      configPath,
      repoRoot,
      extensionPath: plan.homeboy_extensions_path || repoRoot,
      replayBundleDir: plan.replay_bundle_dir || process.env.HOMEBOY_RUNTIME_AGENT_REPLAY_BUNDLE_DIR,
      env: process.env,
      validationPolicy: {
        scenario_id: plan.workload_id,
        success_requires_pr: plan.success_requires_pr,
        success_completion_outcomes: plan.success_completion_outcomes,
      },
    });
    writeHeadlessDeterministicLoopArtifacts({
      ...plan,
      result,
      env: process.env,
    });
    process.stdout.write(`${JSON.stringify(result.outcome, null, 2)}\n`);
    process.exitCode = result.status === 'succeeded' && ['succeeded', 'no_op'].includes(result.outcome?.status) ? 0 : 1;
    return;
  }

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
}()).catch((error) => {
  process.stderr.write(`${error && error.message ? error.message : String(error)}\n`);
  process.exitCode = 1;
});
