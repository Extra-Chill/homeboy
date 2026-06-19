'use strict';

/* eslint-disable camelcase */

/**
 * External dependencies
 */
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
  DEFAULT_RUNTIME_ID,
  resolveRuntimeProvider,
} = require('../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');

const REPO_ROOT = path.resolve(__dirname, '..', '..');
const DEFAULT_TASK_TIMEOUT_SECONDS = 45 * 60;

function auditFanoutRuntimeId(options = {}) {
  return options.runtimeProvider
    || options.runtime_provider
    || options.runtime
    || options.runtimeId
    || options.runtime_id
    || options.agentRuntime
    || options.agent_runtime
    || process.env.AGENT_RUNTIME
    || DEFAULT_RUNTIME_ID;
}

function auditFanoutRuntimeProvider(options = {}) {
  return resolveRuntimeProvider(auditFanoutRuntimeId(options), {
    repoRoot: options.repoRoot || REPO_ROOT,
    registry: options.registry,
    workspace: options.workspace || options.cwd || process.cwd(),
    env: options.env,
  });
}

function auditFanoutRuntimeInvocation(options = {}) {
  const runtimeProvider = options.runtimeProviderObject || options.runtime_provider_object || auditFanoutRuntimeProvider(options);
  const explicitCommand = options.command || options.runtime_command;
  const explicitArgs = options.args || options.runtime_args || [];

  if (explicitCommand) {
    return {
      runtime: runtimeProvider,
      command: explicitCommand,
      args: explicitArgs,
    };
  }

  if (runtimeProvider.id === DEFAULT_RUNTIME_ID) {
    return {
      runtime: runtimeProvider,
      command: 'wp-codebox',
      args: explicitArgs,
    };
  }

  if (runtimeProvider.executor?.path) {
    return {
      runtime: runtimeProvider,
      command: process.execPath,
      args: [runtimeProvider.executor.path, ...explicitArgs],
    };
  }

  return {
    runtime: runtimeProvider,
    command: runtimeProvider.executor?.backend || runtimeProvider.id,
    args: explicitArgs,
  };
}

function auditFanoutRuntimeEnv(taskRequest, requestJson, options = {}) {
  const runtimeEnv = typeof options.runtime_env === 'function'
    ? options.runtime_env(taskRequest, requestJson, options)
    : (options.runtime_env || {});

  return {
    ...process.env,
    ...(options.env || {}),
    HOMEBOY_AGENT_TASK_REQUEST: requestJson,
    HOMEBOY_AGENT_TASK_ID: taskRequest.task_id || taskRequest.id || taskRequest.sandbox_session_id || '',
    HOMEBOY_AGENT_TASK_GROUP_KEY: taskRequest.group_key || '',
    ...runtimeEnv,
  };
}

function executeAuditFanoutRuntimeTaskSync(taskRequest, options = {}) {
  const invocation = auditFanoutRuntimeInvocation(options);
  const requestJson = `${JSON.stringify(taskRequest, null, 2)}\n`;
  const startedAt = new Date().toISOString();
  const result = spawnSync(invocation.command, invocation.args, {
    cwd: options.cwd || process.cwd(),
    encoding: 'utf8',
    env: auditFanoutRuntimeEnv(taskRequest, requestJson, options),
    input: requestJson,
    maxBuffer: options.max_buffer || 1024 * 1024 * 10,
  });
  const finishedAt = new Date().toISOString();

  return {
    runtime: invocation.runtime,
    command: invocation.command,
    args: invocation.args,
    result,
    started_at: startedAt,
    finished_at: finishedAt,
    stdout: result.stdout || '',
    stderr: result.stderr || '',
    timed_out: false,
    timeout_seconds: 0,
    killed_process_group: false,
    force_killed_process_group: false,
  };
}

function executeAuditFanoutRuntimeTask(taskRequest, options = {}) {
  const invocation = auditFanoutRuntimeInvocation(options);
  const requestJson = `${JSON.stringify(taskRequest, null, 2)}\n`;
  const startedAt = new Date().toISOString();
  const taskTimeoutSeconds = normalizeTaskTimeoutSeconds(options.task_timeout_seconds);

  return new Promise((resolve) => {
    const child = spawn(invocation.command, invocation.args, {
      cwd: options.cwd || process.cwd(),
      detached: true,
      env: auditFanoutRuntimeEnv(taskRequest, requestJson, options),
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    let spawnError = null;
    let timedOut = false;
    let timeout = null;
    let forceKillTimeout = null;
    let killedProcessGroup = false;
    let forceKilledProcessGroup = false;
    const maxBuffer = options.max_buffer || 1024 * 1024 * 10;

    if (taskTimeoutSeconds > 0) {
      timeout = setTimeout(() => {
        timedOut = true;
        killedProcessGroup = killProcessTree(child, 'SIGTERM');
        forceKillTimeout = setTimeout(() => {
          forceKilledProcessGroup = killProcessTree(child, 'SIGKILL');
        }, 5000);
        forceKillTimeout.unref?.();
      }, taskTimeoutSeconds * 1000);
      timeout.unref?.();
    }

    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
      if (stdout.length > maxBuffer) {
        child.kill();
      }
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk;
      if (stderr.length > maxBuffer) {
        child.kill();
      }
    });
    child.on('error', (error) => {
      spawnError = error;
    });
    child.on('close', (code, signal) => {
      if (timeout) {
        clearTimeout(timeout);
      }
      if (forceKillTimeout) {
        clearTimeout(forceKillTimeout);
      }
      resolve({
        runtime: invocation.runtime,
        command: invocation.command,
        args: invocation.args,
        result: {
          status: code,
          signal: signal || null,
          error: spawnError,
        },
        started_at: startedAt,
        finished_at: new Date().toISOString(),
        stdout,
        stderr,
        timed_out: timedOut,
        timeout_seconds: taskTimeoutSeconds,
        killed_process_group: killedProcessGroup,
        force_killed_process_group: forceKilledProcessGroup,
      });
    });
    child.stdin.end(requestJson);
  });
}

function normalizeTaskTimeoutSeconds(value) {
  const parsed = Number.parseInt(value || DEFAULT_TASK_TIMEOUT_SECONDS, 10);
  if (!Number.isFinite(parsed) || parsed < 1) {
    return DEFAULT_TASK_TIMEOUT_SECONDS;
  }
  return parsed;
}

function killProcessTree(child, signal) {
  if (!child.pid) {
    return child.kill(signal);
  }

  try {
    process.kill(-child.pid, signal);
    return true;
  } catch {
    return child.kill(signal);
  }
}

module.exports = {
  DEFAULT_TASK_TIMEOUT_SECONDS,
  auditFanoutRuntimeId,
  auditFanoutRuntimeEnv,
  auditFanoutRuntimeInvocation,
  auditFanoutRuntimeProvider,
  executeAuditFanoutRuntimeTask,
  executeAuditFanoutRuntimeTaskSync,
  normalizeTaskTimeoutSeconds,
};
