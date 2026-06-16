#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const SCRIPT_DIR = __dirname;
const EXTENSION_PATH = path.resolve(SCRIPT_DIR, '..', '..');
const EXECUTOR = path.join(SCRIPT_DIR, 'homeboy-codebox-agent-task-executor.cjs');

function readConfigPath() {
  const configPath = process.argv[2] || process.env.HOMEBOY_DATAMACHINE_AGENT_CONFIG_PATH || '';
  if (!configPath) {
    throw new Error('Pass a Data Machine agent config JSON path as argv[1] or HOMEBOY_DATAMACHINE_AGENT_CONFIG_PATH.');
  }
  return configPath;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function nonEmpty(value) {
  return value !== undefined && value !== null && value !== '';
}

function optionalObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function artifactDeclarationName(declaration) {
  if (typeof declaration === 'string') {
    return declaration;
  }
  if (!declaration || typeof declaration !== 'object' || Array.isArray(declaration)) {
    return '';
  }
  return declaration.name || declaration.id || '';
}

function expectedArtifactsFromConfig(config) {
  const expected = normalizeArray(config.expected_artifacts);
  if (expected.length > 0) {
    return expected;
  }
  return normalizeArray(config.artifact_declarations)
    .filter((declaration) => declaration && typeof declaration === 'object' && declaration.required === true)
    .map(artifactDeclarationName)
    .filter(Boolean);
}

function buildAgentTaskRequest(config, configPath) {
  const taskId = config.task_id || config.workload_id || config.flow_slug || 'datamachine-agent-ci';
  const timeoutMs = Number.parseInt(config.time_budget_ms || '', 10);
  const timeoutSeconds = Number.parseInt(config.task_timeout_seconds || config.taskTimeoutSeconds || '', 10);
  const runtimeComponents = optionalObject(config.runtime_components);
  const runtimeComponentPaths = Object.fromEntries(Object.entries({
    ...optionalObject(config.runtime_component_paths),
    agents_api: config.agents_api || config.agents_api_path || runtimeComponents.agents_api,
    agent_runtime: config.agent_runtime || config.agent_runtime_path || runtimeComponents.data_machine,
    agent_runtime_tools: config.agent_runtime_tools || config.agent_runtime_tools_path || runtimeComponents.data_machine_code,
    runtime: runtimeComponents.runtime,
  }).filter(([, value]) => nonEmpty(value)));
  const executorConfig = Object.fromEntries(Object.entries({
    ...config,
    execution_kind: config.execution_kind || 'agent_bundle',
    agents_api: config.agents_api || config.agents_api_path || runtimeComponents.agents_api,
    runtime_component_paths: runtimeComponentPaths,
    homeboy_extensions: config.homeboy_extensions || config.homeboy_extensions_path || EXTENSION_PATH,
    artifacts: config.artifacts_path || config.artifacts,
    replay_bundle_dir: config.replay_bundle_dir || process.env.HOMEBOY_DATAMACHINE_AGENT_REPLAY_BUNDLE_DIR,
  }).filter(([, value]) => nonEmpty(value)));

  return {
    schema: 'homeboy/agent-task-request/v1',
    task_id: String(taskId),
    group_key: config.group_key || config.flow_slug || config.pipeline_slug || '',
    instructions: config.prompt || config.workload_label || 'Run Data Machine agent CI task.',
    source_refs: [{ kind: 'config', path: configPath }],
    workspace: Object.fromEntries(Object.entries({
      repository: config.target_repo,
      path: config.component_path,
    }).filter(([, value]) => nonEmpty(value))),
    expected_artifacts: expectedArtifactsFromConfig(config),
    artifact_declarations: normalizeArray(config.artifact_declarations),
    policy: optionalObject(config.policy),
    limits: Object.fromEntries(Object.entries({
      timeout_ms: Number.isFinite(timeoutMs) && timeoutMs > 0 ? timeoutMs : undefined,
      task_timeout_seconds: Number.isFinite(timeoutSeconds) && timeoutSeconds > 0 ? timeoutSeconds : undefined,
    }).filter(([, value]) => nonEmpty(value))),
    inputs: {
      target: Object.fromEntries(Object.entries({
        repository: config.target_repo,
        path: config.component_path,
      }).filter(([, value]) => nonEmpty(value))),
      context: {
        config_path: configPath,
        workflow_run_url: process.env.GITHUB_SERVER_URL && process.env.GITHUB_REPOSITORY && process.env.GITHUB_RUN_ID
          ? `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/actions/runs/${process.env.GITHUB_RUN_ID}`
          : '',
      },
    },
    executor: {
      backend: 'codebox',
      model: config.model || '',
      config: executorConfig,
      secret_env: config.secret_env || [],
    },
  };
}

function scenarioResultsFromOutcome(outcome) {
  const codebox = outcome?.metadata?.codebox || {};
  const workload = codebox?.raw?.agent_runtime?.result
    || codebox?.agent_runtime?.workload
    || codebox?.agent_runtime?.result
    || codebox?.metadata?.agent_runtime?.workload
    || codebox?.metadata?.agent_runtime?.result
    || codebox?.agentResult
    || codebox?.agent_result
    || null;

  if (workload && Array.isArray(workload.scenarios)) {
    return workload;
  }

  return {
    scenarios: [{
      id: outcome.task_id || 'datamachine-agent-ci',
      label: outcome.summary || 'Data Machine agent CI task',
      metrics: { generic_agent_task_executor_mean: 1 },
      metadata: {
        job_status: outcome.status,
        success_status: outcome.status,
        agent_task_outcome: outcome,
      },
    }],
  };
}

function writeLegacyResults(outcome) {
  const resultsFile = process.env.HOMEBOY_DATAMACHINE_AGENT_RESULTS_FILE || '';
  if (!resultsFile) {
    return;
  }
  fs.mkdirSync(path.dirname(resultsFile), { recursive: true });
  fs.writeFileSync(resultsFile, `${JSON.stringify(scenarioResultsFromOutcome(outcome), null, 2)}\n`);
}

function writeOutcome(outcome) {
  const outcomeFile = process.env.HOMEBOY_AGENT_TASK_OUTCOME_FILE || '';
  if (!outcomeFile) {
    return;
  }
  fs.mkdirSync(path.dirname(outcomeFile), { recursive: true });
  fs.writeFileSync(outcomeFile, `${JSON.stringify(outcome, null, 2)}\n`);
}

try {
  const configPath = readConfigPath();
  const config = readJson(configPath);
  const request = buildAgentTaskRequest(config, configPath);
  const result = spawnSync(process.execPath, [EXECUTOR], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: process.env,
    maxBuffer: 1024 * 1024 * 20,
  });

  if (result.stderr) {
    process.stderr.write(result.stderr);
  }

  const outcome = result.stdout.trim() ? JSON.parse(result.stdout) : {
    schema: 'homeboy/agent-task-outcome/v1',
    task_id: request.task_id,
    status: 'failed',
    summary: 'WP Codebox agent task executor produced no JSON outcome.',
    diagnostics: [{
      class: 'homeboy.datamachine_agent_task.no_outcome',
      message: 'WP Codebox agent task executor produced no JSON outcome.',
      data: { exit_status: result.status ?? 1 },
    }],
  };

  writeOutcome(outcome);
  writeLegacyResults(outcome);
  process.stdout.write(`${JSON.stringify(outcome, null, 2)}\n`);
  process.exitCode = outcome.status === 'succeeded' || outcome.status === 'no_op' ? 0 : 1;
} catch (error) {
  console.error(error && error.message ? error.message : String(error));
  process.exitCode = 1;
}
