'use strict';

const path = require('node:path');

const ARTIFACT_PATHS_SCHEMA = 'homeboy/runtime-agent-artifact-paths/v1';

function runtimeAgentArtifactPaths(options = {}) {
  const provided = options.artifact_paths && typeof options.artifact_paths === 'object' && !Array.isArray(options.artifact_paths) ? options.artifact_paths : {};
  const runDir = firstString(
    provided.run_dir,
    provided.runDir,
    options.runDir,
    options.run_dir,
    options.artifactsDir,
    options.artifacts_dir,
    options.artifactsPath,
    options.artifacts_path,
    options.plan?.artifacts_path,
    options.plan?.artifacts,
    options.env?.HOMEBOY_RUNTIME_AGENT_RUN_DIR,
    options.env?.HOMEBOY_RUNTIME_AGENT_ARTIFACTS_DIR,
    options.env?.HOMEBOY_RUNTIME_AGENT_ARTIFACTS,
    process.env.HOMEBOY_RUNTIME_AGENT_RUN_DIR,
    process.env.HOMEBOY_RUNTIME_AGENT_ARTIFACTS_DIR,
    process.env.HOMEBOY_RUNTIME_AGENT_ARTIFACTS
  );
  return stripUndefined({
    schema: ARTIFACT_PATHS_SCHEMA,
    run_dir: runDir,
    events: firstString(provided.events, options.eventsFile, options.events_file, options.env?.HOMEBOY_RUNTIME_AGENT_EVENTS_FILE, process.env.HOMEBOY_RUNTIME_AGENT_EVENTS_FILE, runDir ? path.join(runDir, 'events.json') : ''),
    status: firstString(provided.status, options.statusFile, options.status_file, options.env?.HOMEBOY_RUNTIME_AGENT_STATUS_FILE, process.env.HOMEBOY_RUNTIME_AGENT_STATUS_FILE, runDir ? path.join(runDir, 'status.json') : ''),
    results: firstString(provided.results, options.resultsFile, options.results_file, options.env?.HOMEBOY_RUNTIME_AGENT_RESULTS_FILE, process.env.HOMEBOY_RUNTIME_AGENT_RESULTS_FILE, runDir ? path.join(runDir, 'results.json') : ''),
    outcome: firstString(provided.outcome, options.outcomeFile, options.outcome_file, options.env?.HOMEBOY_AGENT_TASK_OUTCOME_FILE, process.env.HOMEBOY_AGENT_TASK_OUTCOME_FILE, runDir ? path.join(runDir, 'outcome.json') : ''),
    stderr: firstString(provided.stderr, options.stderrFile, options.stderr_file, options.env?.HOMEBOY_RUNTIME_AGENT_STDERR_FILE, process.env.HOMEBOY_RUNTIME_AGENT_STDERR_FILE),
    fanout_run: firstString(provided.fanout_run, provided.fanoutRun, options.fanoutRunFile, options.fanout_run_file, options.runsOutputPath, options.runs_output_path, options.env?.HOMEBOY_RUNTIME_AGENT_FANOUT_RUN_FILE, process.env.HOMEBOY_RUNTIME_AGENT_FANOUT_RUN_FILE, runDir ? path.join(runDir, 'fanout-run.json') : ''),
    loop_result: firstString(provided.loop_result, provided.loopResult, options.loopResultFile, options.loop_result_file, options.resultFile, options.result_file, options.env?.HOMEBOY_RUNTIME_AGENT_LOOP_RESULT_FILE, process.env.HOMEBOY_RUNTIME_AGENT_LOOP_RESULT_FILE, runDir ? path.join(runDir, 'loop-result.json') : ''),
    loop_policy: firstString(provided.loop_policy, provided.loopPolicy, options.loopPolicyFile, options.loop_policy_file, options.env?.HOMEBOY_RUNTIME_AGENT_LOOP_POLICY_FILE, process.env.HOMEBOY_RUNTIME_AGENT_LOOP_POLICY_FILE, runDir ? path.join(runDir, 'loop-policy.json') : ''),
  });
}

function firstString(...values) {
  for (const value of values) {
    if (typeof value === 'string' && value.trim() !== '') {
      return value.trim();
    }
  }
  return '';
}

function stripUndefined(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined && entry !== ''));
}

module.exports = {
  ARTIFACT_PATHS_SCHEMA,
  runtimeAgentArtifactPaths,
};
