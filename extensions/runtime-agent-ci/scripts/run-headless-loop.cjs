#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {
  runHeadlessDeterministicLoop,
  writeHeadlessDeterministicLoopArtifacts,
} = require('../lib/headless-deterministic-loop-runner');
const {
  materializeHeadlessProductionLoopSpec,
  parseJsonArray,
  parseJsonObject,
} = require('../lib/headless-production-loop-spec');

(async () => {
let args = {};
let rawSpec = null;
let spec = null;
let result = null;
try {
  args = parseArgs(process.argv.slice(2));
  const specPath = args.spec || args.config || process.env.HOMEBOY_RUNTIME_AGENT_CONFIG_PATH || '';
  if (!specPath) {
    throw new Error('Pass --spec <path>, --config <path>, or HOMEBOY_RUNTIME_AGENT_CONFIG_PATH.');
  }
  const repoRoot = path.resolve(__dirname, '..', '..');
  rawSpec = JSON.parse(fs.readFileSync(specPath, 'utf8'));
  // Fill required secret env names from declared fallbacks before any preflight
  // runs. The provider's canonical key (e.g. OPENAI_API_KEY) is sourced from the
  // caller's generic credential secret, and the Homeboy app token falls back to
  // the repository GITHUB_TOKEN when no app token is present. Forwarding still
  // happens through the secret-env-names-only boundary; only this process's env
  // is populated so the names resolve.
  applySecretEnvFallbacks(rawSpec.secret_env_fallbacks);
  spec = materializeHeadlessProductionLoopSpec(rawSpec, {
    revolutions: args.revolutions || args.max_revolutions || process.env.HOMEBOY_HEADLESS_LOOP_REVOLUTIONS,
    runtime_id: args.runtime_id || process.env.HOMEBOY_AGENT_RUNTIME,
    runtime_profile: args.runtime_profile || process.env.HOMEBOY_AGENT_RUNTIME_PROFILE,
    runtime_profiles: parseJsonObject(args.runtime_profiles || process.env.HOMEBOY_AGENT_RUNTIME_PROFILES, 'runtime_profiles'),
    provider: args.provider || process.env.HOMEBOY_AGENT_RUNTIME_PROVIDER,
    model: args.model || process.env.HOMEBOY_AGENT_RUNTIME_MODEL,
    provider_plugin_paths: listArg(args.provider_plugin_paths || process.env.HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATHS),
    provider_plugins: parseJsonArray(args.provider_plugins || process.env.HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGINS, 'provider_plugins'),
    secret_env: listArg(args.secret_env || process.env.HOMEBOY_AGENT_RUNTIME_SECRET_ENV),
    runtime_env: parseJsonObject(args.runtime_env || process.env.HOMEBOY_AGENT_RUNTIME_ENV, 'runtime_env'),
    runtime_config_mounts: parseJsonArray(args.runtime_config_mounts || process.env.HOMEBOY_AGENT_RUNTIME_CONFIG_MOUNTS, 'runtime_config_mounts'),
    runtime_state_mounts: parseJsonArray(args.runtime_state_mounts || process.env.HOMEBOY_AGENT_RUNTIME_STATE_MOUNTS, 'runtime_state_mounts'),
  });
  result = await runHeadlessDeterministicLoop({
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
    exitCode: result.status === 'succeeded' ? 0 : 1,
  });
  process.stdout.write(`${JSON.stringify(result.outcome || result, null, 2)}\n`);
  process.exitCode = result.status === 'succeeded' ? 0 : 1;
} catch (error) {
  result = failureResult(error, spec || rawSpec);
  try {
    writeHeadlessDeterministicLoopArtifacts({
      ...(spec || rawSpec || {}),
      result,
      outcomeFile: args.outcome || process.env.HOMEBOY_AGENT_TASK_OUTCOME_FILE || '',
      resultsFile: args.results || process.env.HOMEBOY_RUNTIME_AGENT_RESULTS_FILE || '',
      eventsFile: args.events || process.env.HOMEBOY_RUNTIME_AGENT_EVENTS_FILE || '',
      loopResultFile: args.result || process.env.HOMEBOY_RUNTIME_AGENT_LOOP_RESULT_FILE || '',
      exitCode: 1,
    });
  } catch (artifactError) {
    process.stderr.write(`failed to write headless loop failure artifacts: ${artifactError && artifactError.message ? artifactError.message : String(artifactError)}\n`);
  }
  process.stderr.write(`${error && error.message ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
})();

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

function listArg(value) {
  return String(value || '').split(',').map((item) => item.trim()).filter(Boolean);
}

function applySecretEnvFallbacks(fallbacks) {
  if (!fallbacks || typeof fallbacks !== 'object' || Array.isArray(fallbacks)) {
    return;
  }
  for (const [target, sources] of Object.entries(fallbacks)) {
    if (typeof target !== 'string' || target === '') {
      continue;
    }
    if (typeof process.env[target] === 'string' && process.env[target] !== '') {
      continue;
    }
    const sourceList = Array.isArray(sources) ? sources : [sources];
    for (const source of sourceList) {
      const value = typeof source === 'string' ? process.env[source] : undefined;
      if (typeof value === 'string' && value !== '') {
        process.env[target] = value;
        break;
      }
    }
  }
}

function failureResult(error, plan = {}) {
  const taskId = plan?.task_id || plan?.workload_id || plan?.loop_id || 'headless-deterministic-loop';
  const now = new Date().toISOString();
  const message = error && error.message ? error.message : String(error);
  const outcome = {
    schema: 'homeboy/agent-task-outcome/v1',
    task_id: taskId,
    status: 'failed',
    summary: message,
    diagnostics: [{ class: 'homeboy.headless_loop.execution_failed', message }],
  };
  return {
    schema: 'homeboy/headless-deterministic-loop-result/v1',
    loop_id: plan?.loop_id || plan?.plan_id || plan?.workload_id || 'headless-deterministic-loop',
    status: 'failed',
    dry_run: plan?.dry_run === true,
    started_at: now,
    completed_at: now,
    runtime: { id: plan?.runtime_id || plan?.runtime || '', backend: '' },
    tasks: [{ task_id: taskId, outcome, results: { scenarios: [] }, loop_policy: null, loop_result: null, state: null }],
    outcome,
    results: { scenarios: [] },
    state: null,
    fanout: { schema: 'homeboy/fanout-reconcile-run/v1', status: 'failed', summary: { total: 1, completed: 0, failed: 1 }, records: [] },
    events: [{ schema: 'homeboy/headless-deterministic-loop-event/v1', sequence: 1, type: 'loop_failed', timestamp: now, error: message }],
  };
}
