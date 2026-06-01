#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const fs = require('node:fs');
const { spawnSync } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
  agentTaskOutcomeFromCodeboxResult,
  codeboxTaskRequestFromAgentTaskRequest,
  providerContract,
} = require('../../lib/codebox-agent-task-executor');

function argValue(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function hasFlag(name) {
  return process.argv.includes(name);
}

function readStdin() {
  try {
    return fs.readFileSync(0, 'utf8');
  } catch {
    return '';
  }
}

function readRequest() {
  const raw = process.env.HOMEBOY_AGENT_TASK_REQUEST || readStdin();
  if (!raw.trim()) {
    throw new Error('AgentTaskRequest JSON is required on stdin or HOMEBOY_AGENT_TASK_REQUEST.');
  }
  return JSON.parse(raw);
}

function requestTimeoutMs(request) {
  const timeoutMs = request?.limits?.timeout_ms || request?.limits?.max_runtime_ms;
  const timeoutSeconds = request?.limits?.task_timeout_seconds || request?.limits?.taskTimeoutSeconds;
  const timeout = timeoutMs || (timeoutSeconds ? Number.parseInt(timeoutSeconds, 10) * 1000 : undefined);
  const parsed = Number.parseInt(timeout, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
}

function timeoutPayload(request, timeoutMs) {
  const artifacts = argValue('--artifacts');
  const evidencePath = artifacts ? `${artifacts}/homeboy-codebox-task-runner.json` : '';
  const knownArtifacts = [];
  if (artifacts) {
    knownArtifacts.push({
      id: 'homeboy-codebox-artifacts',
      kind: 'codebox-artifact-directory',
      path: artifacts,
      metadata: { evidencePath },
    });
  }
  if (evidencePath && fs.existsSync(evidencePath)) {
    knownArtifacts.push({
      id: 'homeboy-codebox-task-runner-preflight',
      kind: 'codebox-task-runner-preflight',
      path: evidencePath,
      metadata: { artifacts },
    });
  }

  return {
    success: false,
    timeout: true,
    summary: `WP Codebox agent task timed out after ${timeoutMs}ms.`,
    artifacts: knownArtifacts,
    evidence_refs: evidencePath && fs.existsSync(evidencePath) ? [{
      kind: 'codebox-task-runner-preflight',
      uri: evidencePath,
      label: 'WP Codebox task runner preflight evidence',
    }] : [],
    diagnostics: [{
      class: 'codebox.timeout',
      message: 'Task runner exceeded the AgentTaskRequest timeout.',
      data: { timeout_ms: timeoutMs, artifacts, evidence_path: evidencePath },
    }],
    metadata: { timeout_ms: timeoutMs, artifacts, evidence_path: evidencePath },
  };
}

function runTaskRunner(request) {
  const runner = argValue('--task-runner') || `${__dirname}/homeboy-wp-codebox-task-runner.cjs`;
  const config = request.executor?.config || {};
  const configArgs = [
    ['--agents-api', config.agents_api_path || config.agentsApiPath],
    ['--data-machine', config.data_machine_path || config.dataMachinePath],
    ['--data-machine-code', config.data_machine_code_path || config.dataMachineCodePath],
    ['--homeboy', config.homeboy_path || config.homeboyPath],
    ['--homeboy-extensions', config.homeboy_extensions_path || config.homeboyExtensionsPath],
  ].flatMap(([name, value]) => (value ? [name, value] : []));
  const args = process.argv.slice(2).filter((arg, index, all) => {
    if (arg === '--task-runner' || all[index - 1] === '--task-runner' || arg === '--print-contract') {
      return false;
    }
    return true;
  });
  const result = spawnSync(process.execPath, [runner, ...args, ...configArgs], {
    encoding: 'utf8',
    input: JSON.stringify(codeboxTaskRequestFromAgentTaskRequest(request)),
    env: process.env,
    maxBuffer: 1024 * 1024 * 20,
    timeout: requestTimeoutMs(request),
  });

  if (result.stderr) {
    process.stderr.write(result.stderr);
  }

  let payload = {};
  if (result.error && result.error.code === 'ETIMEDOUT') {
    payload = timeoutPayload(request, requestTimeoutMs(request));
    return agentTaskOutcomeFromCodeboxResult(request, payload, { exitStatus: 1 });
  }
  if (result.stdout.trim()) {
    try {
      payload = JSON.parse(result.stdout);
    } catch {
      payload = {
        success: result.status === 0,
        summary: result.stdout.trim(),
        diagnostics: [{ class: 'codebox.stdout', message: 'WP Codebox returned non-JSON stdout.', data: {} }],
      };
    }
  }

  return agentTaskOutcomeFromCodeboxResult(request, payload, { exitStatus: result.status ?? 1 });
}

try {
  if (hasFlag('--print-contract')) {
    process.stdout.write(`${JSON.stringify(providerContract(), null, 2)}\n`);
    process.exit(0);
  }

  const request = readRequest();
  const outcome = runTaskRunner(request);
  process.stdout.write(`${JSON.stringify(outcome, null, 2)}\n`);
  process.exitCode = outcome.status === 'succeeded' || outcome.status === 'no_op' ? 0 : 1;
} catch (error) {
  console.error(error && error.message ? error.message : String(error));
  process.exitCode = 1;
}
