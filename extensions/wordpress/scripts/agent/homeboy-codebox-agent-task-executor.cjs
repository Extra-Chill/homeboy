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

function runTaskRunner(request) {
  const runner = argValue('--task-runner') || `${__dirname}/homeboy-wp-codebox-task-runner.cjs`;
  const args = process.argv.slice(2).filter((arg, index, all) => {
    if (arg === '--task-runner' || all[index - 1] === '--task-runner' || arg === '--print-contract') {
      return false;
    }
    return true;
  });
  const result = spawnSync(process.execPath, [runner, ...args], {
    encoding: 'utf8',
    input: JSON.stringify(codeboxTaskRequestFromAgentTaskRequest(request)),
    env: process.env,
    maxBuffer: 1024 * 1024 * 20,
  });

  if (result.stderr) {
    process.stderr.write(result.stderr);
  }

  let payload = {};
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
