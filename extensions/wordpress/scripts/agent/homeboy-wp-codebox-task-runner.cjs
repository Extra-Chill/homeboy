#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

function argValue(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function argValues(name) {
  const values = [];
  for (let index = 0; index < process.argv.length; index += 1) {
    if (process.argv[index] === name && process.argv[index + 1]) {
      values.push(process.argv[index + 1]);
    }
  }
  return values;
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

function readTaskRequest() {
  const raw = process.env.HOMEBOY_WP_CODEBOX_TASK_REQUEST || readStdin();
  if (!raw.trim()) {
    throw new Error('Task request JSON is required on stdin or HOMEBOY_WP_CODEBOX_TASK_REQUEST.');
  }
  const request = JSON.parse(raw);
  if (!request || request.schema !== 'homeboy/wp-codebox-task-request/v1') {
    throw new Error('Task request must use schema homeboy/wp-codebox-task-request/v1.');
  }
  return request;
}

function secretEnvNames(request) {
  return Array.from(new Set([...(request.secret_env || []), ...argValues('--secret-env')].filter(Boolean)));
}

function assertRequiredSecretEnvAvailable(request) {
  const missing = secretEnvNames(request).filter((name) => !process.env[name]);
  if (missing.length > 0) {
    throw new Error(`Required WP Codebox secret environment variable missing: ${missing.join(', ')}`);
  }
}

function mountEntryFromValue(value, metadata) {
  const [source, target, mode = 'readwrite'] = value.split(':');
  if (!source || !target) {
    throw new Error(`Invalid --mount value: ${value}`);
  }
  return {
    source,
    target,
    mode,
    metadata,
  };
}

function mountEntries(request) {
  return [
    ...(request.mounts || []),
    ...argValues('--mount').map((value) => mountEntryFromValue(value, { kind: 'homeboy-audit-fanout' })),
  ];
}

function runtimeStackMountEntries(request) {
  return [
    ...(request.runtime_stack_mounts || []),
    ...argValues('--runtime-stack-mount').map((value) => mountEntryFromValue(value, { kind: 'homeboy-runtime-stack' })),
  ];
}

function runtimeOverlayEntries(request) {
  return [
    ...(request.runtime_overlays || []),
    ...argValues('--runtime-overlay-json').map((value) => JSON.parse(value)),
  ];
}

function realPathForContainment(filePath) {
  const resolved = path.resolve(filePath);
  if (fs.existsSync(resolved)) {
    return fs.realpathSync(resolved);
  }
  return path.join(fs.realpathSync(path.dirname(resolved)), path.basename(resolved));
}

function pathInside(parent, candidate) {
  try {
    const parentReal = realPathForContainment(parent);
    const candidateReal = realPathForContainment(candidate);
    const relative = path.relative(parentReal, candidateReal);
    return relative === '' || (Boolean(relative) && !relative.startsWith('..') && !path.isAbsolute(relative));
  } catch {
    return false;
  }
}

function requestTimeoutMs(request) {
  const seconds = Number.parseInt(request.task_timeout_seconds || request.taskTimeoutSeconds || argValue('--task-timeout-seconds'), 10);
  return Number.isFinite(seconds) && seconds > 0 ? seconds * 1000 : undefined;
}

function executable(filePath) {
  try {
    fs.accessSync(filePath, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function resolveCommand(command, args) {
  if ((path.extname(command) === '.js' || path.extname(command) === '.cjs' || path.extname(command) === '.mjs') && !executable(command)) {
    return { command: process.execPath, args: [command, ...args] };
  }
  return { command, args };
}

function writeJsonFile(prefix, value) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  const filePath = path.join(directory, 'input.json');
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
  return filePath;
}

function writePreflightEvidence(artifacts, evidence) {
  try {
    fs.mkdirSync(artifacts, { recursive: true });
    const evidencePath = path.join(artifacts, 'homeboy-codebox-task-runner.json');
    fs.writeFileSync(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
    return evidencePath;
  } catch {
    return '';
  }
}

function runnerInput(request, artifacts) {
  return Object.fromEntries(Object.entries({
    parent_request: request,
    provider: argValue('--provider') || request.provider || '',
    model: argValue('--model') || request.model || '',
    provider_plugin_paths: [...(request.provider_plugin_paths || []), ...argValues('--provider-plugin-path')],
    secret_env: secretEnvNames(request),
    mounts: mountEntries(request),
    runtime_stack_mounts: runtimeStackMountEntries(request),
    runtime_overlays: runtimeOverlayEntries(request),
    max_turns: Number.parseInt(argValue('--max-turns') || request.max_turns || request.maxTurns || 0, 10) || undefined,
    task_timeout_seconds: Number.parseInt(argValue('--task-timeout-seconds') || request.task_timeout_seconds || request.taskTimeoutSeconds || 0, 10) || undefined,
    sandbox_session_id: request.sandbox_session_id || '',
    orchestrator: request.orchestrator || {},
    artifacts_path: artifacts,
    wp_codebox_bin: argValue('--wp-codebox-bin') || request.wp_codebox_bin || '',
    agents_api_path: argValue('--agents-api') || request.agents_api || '',
    data_machine_path: argValue('--data-machine') || request.data_machine || '',
    data_machine_code_path: argValue('--data-machine-code') || request.data_machine_code || '',
  }).filter(([, value]) => value !== '' && value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function timeoutPayload(timeoutMs, artifacts, evidencePath, inputPath, command, args) {
  const artifact = evidencePath ? [{
    id: 'homeboy-codebox-task-runner-preflight',
    kind: 'codebox-task-runner-preflight',
    path: evidencePath,
    metadata: { inputPath, artifacts, command, args },
  }] : [];
  return {
    success: false,
    timeout: true,
    summary: `WP Codebox run-agent-task timed out after ${timeoutMs}ms.`,
    artifacts: artifact,
    evidence_refs: evidencePath ? [{
      kind: 'codebox-task-runner-preflight',
      uri: evidencePath,
      label: 'WP Codebox task runner preflight evidence',
    }] : [],
    diagnostics: [{
      class: 'codebox.run_agent_task.timeout',
      message: 'wp codebox run-agent-task exceeded the configured task timeout.',
      data: { timeout_ms: timeoutMs, inputPath, artifacts },
    }],
    metadata: { timeout_ms: timeoutMs, inputPath, artifacts, command, args },
  };
}

function runWpCodeboxParentTask(request) {
  const explicitArtifacts = argValue('--artifacts') || request.artifacts || '';
  const artifacts = explicitArtifacts || fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-artifacts-'));
  if (explicitArtifacts) {
    const riskyMount = mountEntries(request).find((mount) => pathInside(mount.source, explicitArtifacts));
    if (riskyMount) {
      console.error(`Warning: WP Codebox artifact directory is inside mounted source ${riskyMount.source} and may be captured recursively: ${explicitArtifacts}`);
    }
  }

  const input = runnerInput(request, artifacts);
  const inputPath = writeJsonFile('homeboy-wp-codebox-parent-task-', input);
  const wpCliBin = argValue('--wp-cli-bin') || request.wp_cli_bin || 'wp';
  const args = ['codebox', 'run-agent-task', '--input-file', inputPath, '--format=json'];
  const previewHold = argValue('--preview-hold');
  if (previewHold) {
    args.push('--preview-hold-seconds', previewHold);
  }
  const previewPublicUrl = argValue('--preview-public-url');
  if (previewPublicUrl) {
    args.push('--preview-public-url', previewPublicUrl);
  }

  const resolved = resolveCommand(wpCliBin, args);
  const timeoutMs = requestTimeoutMs(request);
  const evidencePath = writePreflightEvidence(artifacts, {
    schema: 'homeboy/wp-codebox-task-runner-preflight/v1',
    inputPath,
    artifacts,
    command: resolved.command,
    args: resolved.args,
    timeout_ms: timeoutMs,
    task_id: request.orchestrator?.agent_task_id,
    sandbox_session_id: request.sandbox_session_id,
  });

  if (hasFlag('--print-command')) {
    console.error(JSON.stringify({ command: resolved.command, args: resolved.args, input }, null, 2));
  }

  const result = spawnSync(resolved.command, resolved.args, {
    encoding: 'utf8',
    env: process.env,
    maxBuffer: 1024 * 1024 * 20,
    timeout: timeoutMs,
  });

  if (result.error && result.error.code === 'ETIMEDOUT') {
    process.stdout.write(`${JSON.stringify(timeoutPayload(timeoutMs, artifacts, evidencePath, inputPath, resolved.command, resolved.args), null, 2)}\n`);
    return 1;
  }

  if (result.stdout) {
    process.stdout.write(result.stdout);
  }
  if (result.stderr) {
    process.stderr.write(result.stderr);
  }
  if (result.error) {
    process.stderr.write(`${result.error.message}\n`);
  }
  return result.status ?? 1;
}

try {
  const request = readTaskRequest();
  assertRequiredSecretEnvAvailable(request);
  process.exitCode = runWpCodeboxParentTask(request);
} catch (error) {
  console.error(error && error.message ? error.message : String(error));
  process.exitCode = 1;
}
