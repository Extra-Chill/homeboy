#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');
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

function readJsonIfAvailable(filePath) {
  try {
    return JSON.parse(fs.readFileSync(filePath, 'utf8'));
  } catch {
    return null;
  }
}

function directorySizeBytes(directory) {
  try {
    return fs.readdirSync(directory, { withFileTypes: true }).reduce((total, entry) => {
      const entryPath = path.join(directory, entry.name);
      if (entry.isDirectory()) {
        return total + directorySizeBytes(entryPath);
      }
      return total + fs.statSync(entryPath).size;
    }, 0);
  } catch {
    return undefined;
  }
}

function fileArtifactKind(filePath) {
  const fileName = path.basename(filePath).toLowerCase();
  if (fileName === 'manifest.json') {
    return 'codebox-artifact-manifest';
  }
  if (fileName === 'runtime-reference-manifest.json') {
    return 'codebox-runtime-reference-manifest';
  }
  const relative = filePath.replace(/\\/g, '/');
  if (/transcript|conversation|messages/.test(relative)) {
    return 'codebox-transcript';
  }
  if (/command|stdout|stderr|console|log/.test(relative)) {
    return 'codebox-command-log';
  }
  if (/heartbeat|status/.test(relative)) {
    return 'codebox-heartbeat';
  }
  if (/phase/.test(relative)) {
    return 'codebox-phase';
  }
  return '';
}

function discoverTimeoutArtifactRefs(artifactsRoot) {
  if (!artifactsRoot || !fs.existsSync(artifactsRoot)) {
    return { artifacts: [], evidenceRefs: [], lastKnownPhase: '', lastHeartbeat: null };
  }

  const discovered = [];
  const queue = [{ filePath: artifactsRoot, depth: 0 }];
  let lastKnownPhase = '';
  let lastHeartbeat = null;
  let runtimeId = '';

  while (queue.length > 0 && discovered.length < 200) {
    const { filePath, depth } = queue.shift();
    let stat;
    try {
      stat = fs.statSync(filePath);
    } catch {
      continue;
    }

    if (stat.isDirectory()) {
      const manifestPath = path.join(filePath, 'manifest.json');
      if (filePath !== artifactsRoot && fs.existsSync(manifestPath)) {
        const manifest = readJsonIfAvailable(manifestPath);
        if (!runtimeId && manifest && typeof manifest === 'object') {
          runtimeId = manifest.runtime_id || manifest.runtimeId || manifest.runtime?.id || '';
        }
        discovered.push({
          id: manifest?.id || manifest?.artifact_id || `codebox-artifact-bundle-${discovered.length + 1}`,
          kind: 'codebox-artifact-bundle',
          path: filePath,
          size_bytes: directorySizeBytes(filePath),
          metadata: manifest && typeof manifest === 'object' ? {
            schema: manifest.schema,
            phase: manifest.phase || manifest.last_phase || manifest.current_phase,
            runtime_id: manifest.runtime_id || manifest.runtimeId || manifest.runtime?.id,
          } : {},
        });
      }
      if (depth < 4) {
        for (const entry of fs.readdirSync(filePath).sort()) {
          queue.push({ filePath: path.join(filePath, entry), depth: depth + 1 });
        }
      }
      continue;
    }

    if (!stat.isFile()) {
      continue;
    }

    const kind = fileArtifactKind(path.relative(artifactsRoot, filePath));
    if (!kind) {
      continue;
    }
    const payload = filePath.endsWith('.json') ? readJsonIfAvailable(filePath) : null;
    if (!lastKnownPhase && payload && typeof payload === 'object') {
      lastKnownPhase = payload.phase || payload.last_phase || payload.lastKnownPhase || payload.current_phase || '';
    }
    if (!runtimeId && payload && typeof payload === 'object') {
      runtimeId = payload.runtime_id || payload.runtimeId || payload.runtime?.id || '';
    }
    if (!lastHeartbeat && payload && typeof payload === 'object' && /heartbeat|status/i.test(filePath)) {
      lastHeartbeat = payload.heartbeat || payload.last_heartbeat || payload;
    }
    discovered.push({
      id: `${kind}-${discovered.length + 1}`,
      kind,
      path: filePath,
      size_bytes: stat.size,
      metadata: payload && typeof payload === 'object' ? {
        schema: payload.schema,
        phase: payload.phase || payload.last_phase || payload.current_phase,
      } : {},
    });
  }

  return {
    artifacts: discovered,
    evidenceRefs: discovered.map((artifact) => ({
      kind: artifact.kind,
      uri: artifact.path,
      label: artifact.kind.replace(/^codebox-/, 'WP Codebox ').replace(/-/g, ' '),
    })),
    runtimeId,
    lastKnownPhase,
    lastHeartbeat,
  };
}

function timeoutPayload(timeoutMs, request = {}) {
  const artifacts = argValue('--artifacts') || request.executor?.config?.artifacts || '';
  const evidencePath = artifacts ? `${artifacts}/homeboy-codebox-task-runner.json` : '';
  const discovered = discoverTimeoutArtifactRefs(artifacts);
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
  for (const artifact of discovered.artifacts) {
    if (!knownArtifacts.some((knownArtifact) => knownArtifact.path === artifact.path)) {
      knownArtifacts.push(artifact);
    }
  }

  const evidenceRefs = evidencePath && fs.existsSync(evidencePath) ? [{
    kind: 'codebox-task-runner-preflight',
    uri: evidencePath,
    label: 'WP Codebox task runner preflight evidence',
  }] : [];
  for (const ref of discovered.evidenceRefs) {
    if (!evidenceRefs.some((knownRef) => knownRef.uri === ref.uri)) {
      evidenceRefs.push(ref);
    }
  }

  return {
    success: false,
    timeout: true,
    summary: `WP Codebox agent task timed out after ${timeoutMs}ms.`,
    artifacts: knownArtifacts,
    evidence_refs: evidenceRefs,
    diagnostics: [{
      class: 'codebox.timeout',
      message: 'Task runner exceeded the AgentTaskRequest timeout.',
      data: {
        timeout_ms: timeoutMs,
        classification: 'provider_timeout',
        artifacts,
        evidence_path: evidencePath,
        artifact_ref_count: knownArtifacts.length,
        runtime_id: discovered.runtimeId,
        last_known_phase: discovered.lastKnownPhase,
        last_heartbeat: discovered.lastHeartbeat,
      },
    }],
    metadata: {
      timeout_ms: timeoutMs,
      timeout_classification: 'provider_timeout',
      artifacts,
      evidence_path: evidencePath,
      artifact_ref_count: knownArtifacts.length,
      runtime_id: discovered.runtimeId,
      last_known_phase: discovered.lastKnownPhase,
      last_heartbeat: discovered.lastHeartbeat,
    },
  };
}

function redactDiagnosticText(text) {
  return String(text || '')
    .replace(/((?:TOKEN|SECRET|PASSWORD|API[_-]?KEY|ACCESS[_-]?KEY|REFRESH[_-]?TOKEN)[A-Z0-9_-]*\s*=\s*)\S+/gi, '$1[redacted]')
    .slice(0, 8000);
}

function missingSecretEnvNames(stderr) {
  const match = String(stderr || '').match(/Required WP Codebox secret environment variable missing:\s*([^\n\r]+)/i);
  if (!match) {
    return [];
  }
  return match[1]
    .split(',')
    .map((name) => name.trim())
    .filter((name) => /^[A-Z0-9_]+$/.test(name));
}

function stderrFailurePayload(result) {
  const stderr = redactDiagnosticText(result.stderr || '');
  const missingSecretEnv = missingSecretEnvNames(stderr);
  const diagnosticClass = missingSecretEnv.length > 0
    ? 'codebox.preflight.missing_secret_env'
    : 'codebox.preflight.stderr';
  const message = missingSecretEnv.length > 0
    ? `WP Codebox task runner preflight is missing required secret environment variables: ${missingSecretEnv.join(', ')}.`
    : 'WP Codebox task runner failed before returning a JSON outcome.';

  return {
    success: false,
    status: 'failed',
    failure_classification: 'provider',
    summary: message,
    diagnostics: [{
      class: diagnosticClass,
      message,
      data: {
        phase: 'codebox.preflight',
        exit_status: result.status ?? 1,
        missing_env: missingSecretEnv,
        stderr,
      },
    }],
    metadata: {
      phase: 'codebox.preflight',
      exit_status: result.status ?? 1,
      missing_env: missingSecretEnv,
      stderr,
    },
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
    payload = timeoutPayload(requestTimeoutMs(request), request);
    return agentTaskOutcomeFromCodeboxResult(request, payload, { exitStatus: 1 });
  }
  if (result.stdout.trim()) {
    try {
      payload = JSON.parse(result.stdout);
    } catch {
      payload = missingSecretEnvNames(result.stderr).length > 0 ? stderrFailurePayload(result) : {
        success: result.status === 0,
        summary: result.stdout.trim(),
        status: result.status === 0 ? 'succeeded' : 'failed',
        failure_classification: result.status === 0 ? undefined : 'provider',
        diagnostics: [{
          class: 'codebox.stdout',
          message: 'WP Codebox returned non-JSON stdout.',
          data: {
            phase: 'codebox.preflight',
            exit_status: result.status ?? 1,
            stderr: redactDiagnosticText(result.stderr || ''),
          },
        }],
        metadata: {
          phase: 'codebox.preflight',
          exit_status: result.status ?? 1,
          stderr: redactDiagnosticText(result.stderr || ''),
        },
      };
    }
  } else if ((result.status ?? 0) !== 0 || (result.stderr || '').trim()) {
    payload = stderrFailurePayload(result);
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
