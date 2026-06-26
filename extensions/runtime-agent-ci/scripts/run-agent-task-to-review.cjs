#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { runtimeAgentArtifactPaths } = require('../lib/artifact-paths.cjs');
const { runAgentTaskToReview } = require('../lib/agent-task-to-review-runner');
const { resolveRuntimeProvider, runtimeIdFromOptions } = require('../lib/runtime-provider-resolver.cjs');

try {
  const args = parseArgs(process.argv.slice(2));
  const configPath = args.config || args.spec || process.env.HOMEBOY_RUNTIME_AGENT_CONFIG_PATH || '';
  if (!configPath) {
    throw new Error('Pass --config <path>, --spec <path>, or HOMEBOY_RUNTIME_AGENT_CONFIG_PATH.');
  }
  const plan = JSON.parse(fs.readFileSync(configPath, 'utf8'));
  const repoRoot = path.resolve(__dirname, '..', '..');
  const runtime = resolveRuntimeProvider(runtimeIdFromOptions({ runtime_id: args.runtime_id || plan.runtime_id || plan.runtime }, process.env), {
    repoRoot,
    workspace: plan.component_path || plan.workspace || process.cwd(),
    executor: plan.executor || {},
  });
  const result = runAgentTaskToReview({
    plan,
    runtime,
    configPath,
    repoRoot,
    extensionPath: repoRoot,
    replayBundleDir: process.env.HOMEBOY_RUNTIME_AGENT_REPLAY_BUNDLE_DIR,
    validationPolicy: {
      scenario_id: plan.workload_id || plan.task_id,
      success_requires_pr: plan.success_requires_pr,
      success_completion_outcomes: plan.success_completion_outcomes,
      controller_loop_proof: plan.controller_loop_proof || plan.validation_policy?.controller_loop_proof,
    },
  });
  writeAgentTaskToReviewArtifacts({ result, ...args });
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  process.exitCode = result.success ? 0 : 1;
} catch (error) {
  process.stderr.write(`${error && error.message ? error.message : String(error)}\n`);
  process.exitCode = 1;
}

function writeAgentTaskToReviewArtifacts(options = {}) {
  const artifactPaths = runtimeAgentArtifactPaths(options);
  writeJsonIfPath(artifactPaths.outcome, options.result?.runtime_result?.outcome || options.result?.outcome || options.result);
  writeJsonIfPath(artifactPaths.results, options.result?.results);
  writeJsonIfPath(artifactPaths.loop_result, options.result);
  writeJsonIfPath(artifactPaths.events, options.result?.runtime_result?.loop?.events || []);
  writeJsonIfPath(artifactPaths.loop_policy, options.result?.runtime_result?.loop?.policy_status || null);
  writeJsonIfPath(artifactPaths.status, {
    schema: 'homeboy/runtime-agent-status/v1',
    status: options.result?.status || 'failed',
    success: options.result?.success === true,
    task_id: options.result?.task_id || '',
    terminal_status: options.result?.terminal_status || '',
    error: options.result?.error || '',
  });
}

function writeJsonIfPath(filePath, value) {
  if (!filePath) {
    return;
  }
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith('--')) {
      continue;
    }
    const key = arg.slice(2).replace(/-/g, '_');
    args[key] = argv[index + 1];
    index += 1;
  }
  return args;
}
