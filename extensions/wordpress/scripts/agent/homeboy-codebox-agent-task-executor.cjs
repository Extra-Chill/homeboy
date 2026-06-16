#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

/**
 * Internal dependencies
 */
const {
  agentTaskOutcomeFromCodeboxResult,
  codeboxTaskRequestFromAgentTaskRequest,
  providerContract,
} = require('../../lib/codebox-agent-task-executor');

const DEFAULT_CODEBOX_CORE_MODULE = '@automattic/wp-codebox-core';
const CODEX_SECRET_ENV = [
  'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
  'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
  'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
];
const CODEX_PROVIDER_PLUGIN_GUIDANCE = 'Codex tasks require a Codex-capable provider plugin checkout, such as the Codex PR branch for ai-provider-for-openai. Released ai-provider-for-openai trunk registers openai, not codex, and unrelated provider defaults such as ai-provider-for-opencode will not work.';

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

function defaultCodexAuthPath() {
  return process.env.HOME ? path.join(process.env.HOME, '.codex', 'auth.json') : '';
}

function jwtExpiration(token) {
  const payload = String(token || '').split('.')[1];
  if (!payload) {
    return '';
  }
  try {
    const decoded = Buffer.from(payload.replace(/-/g, '+').replace(/_/g, '/'), 'base64').toString('utf8');
    const parsed = JSON.parse(decoded);
    return parsed?.exp ? String(parsed.exp) : '';
  } catch {
    return '';
  }
}

function codexAuthEnv(taskInput) {
  if (taskInput?.provider !== 'codex') {
    return {};
  }
  if (CODEX_SECRET_ENV.slice(0, 4).every((name) => Boolean(process.env[name]))) {
    return {};
  }
  const authPath = process.env.HOMEBOY_WP_CODEBOX_CODEX_AUTH_PATH || defaultCodexAuthPath();
  const auth = readJsonIfAvailable(authPath);
  const tokens = auth?.tokens || {};
  if (!tokens.access_token || !tokens.refresh_token || !tokens.account_id) {
    return {};
  }
  return Object.fromEntries(Object.entries({
    AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: tokens.access_token,
    AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: tokens.refresh_token,
    AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: tokens.expires_at || tokens.expiresAt || jwtExpiration(tokens.access_token),
    AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: tokens.account_id,
    AI_PROVIDER_OPENAI_CODEX_FEDRAMP: tokens.fedramp === undefined ? 'false' : String(tokens.fedramp),
  }).filter(([name, value]) => CODEX_SECRET_ENV.includes(name) && value !== undefined && value !== null && value !== ''));
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
  const match = String(stderr || '').match(/(?:Required WP Codebox secret environment variable missing|Claude Code provider auth preflight failed: missing required secret environment (?:mapping|value)):\s*([^\n\r]+)/i);
  if (!match) {
    return [];
  }
  return match[1]
    .split(',')
    .map((name) => name.trim())
    .filter((name) => /^[A-Z0-9_]+$/.test(name));
}

function preflightFailureClass(stderr, missingSecretEnv) {
  if (/Claude Code provider auth preflight failed:/i.test(stderr)) {
    return missingSecretEnv.length > 0
      ? 'codebox.preflight.claude_code_auth'
      : 'codebox.preflight.stderr';
  }
  return missingSecretEnv.length > 0
    ? 'codebox.preflight.missing_secret_env'
    : 'codebox.preflight.stderr';
}

function stderrFailurePayload(result) {
  const stderr = redactDiagnosticText(result.stderr || '');
  const missingSecretEnv = missingSecretEnvNames(stderr);
  const diagnosticClass = preflightFailureClass(stderr, missingSecretEnv);
  const message = missingSecretEnv.length > 0
    ? `WP Codebox task runner preflight is missing required secret environment variables for ${diagnosticClass === 'codebox.preflight.claude_code_auth' ? 'Claude Code provider auth' : 'the runtime'}: ${missingSecretEnv.join(', ')}.`
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

function isRepoCookingRequest(request, taskInput) {
  const config = request.executor?.config || {};
  const parentConfig = taskInput.parent_request?.executor?.config || {};
  return config.task_kind === 'repo-cooking' || config.taskKind === 'repo-cooking' || parentConfig.task_kind === 'repo-cooking' || parentConfig.taskKind === 'repo-cooking';
}

function workspaceMounts(taskInput) {
  return Array.isArray(taskInput.mounts) ? taskInput.mounts : [];
}

function hasWorkspaceMount(taskInput) {
  return workspaceMounts(taskInput).some((mount) => {
    if (!mount || typeof mount !== 'object') {
      return false;
    }
    if (mount.metadata?.kind === 'homeboy-dmc-workspace') {
      return Boolean(mount.source);
    }
    return Boolean(mount.source) && /^\/workspace(?:\/|$)/.test(String(mount.target || ''));
  });
}

function missingWorkspacePayload(request, taskInput) {
  if (!isRepoCookingRequest(request, taskInput) || hasWorkspaceMount(taskInput)) {
    return null;
  }

  const config = request.executor?.config || {};
  const message = 'WP Codebox repo-cooking task has no mounted checkout/workspace.';
  return {
    success: false,
    status: 'failed',
    failure_classification: 'execution_failed',
    summary: message,
    diagnostics: [{
      class: 'codebox.preflight.missing_workspace',
      message,
      data: {
        phase: 'codebox.preflight',
        repo: config.repo || request.group_key || request.workspace?.slug || '',
        task_kind: config.task_kind || config.taskKind || '',
        workspace_root: config.workspace_root || config.workspaceRoot || request.workspace?.root || '',
        mounts_count: workspaceMounts(taskInput).length,
        workspaces_count: Array.isArray(taskInput.workspaces) ? taskInput.workspaces.length : 0,
        workspace_materialization: request.workspace?.materialization || {},
      },
    }],
    metadata: {
      phase: 'codebox.preflight',
      missing_workspace: true,
      repo: config.repo || request.group_key || request.workspace?.slug || '',
      task_kind: config.task_kind || config.taskKind || '',
      workspace_root: config.workspace_root || config.workspaceRoot || request.workspace?.root || '',
      mounts_count: workspaceMounts(taskInput).length,
      workspaces_count: Array.isArray(taskInput.workspaces) ? taskInput.workspaces.length : 0,
      workspace_materialization: request.workspace?.materialization || {},
    },
  };
}

function normalizeStringArray(value) {
  if (Array.isArray(value)) {
    return value.filter((item) => typeof item === 'string' && item.trim()).map((item) => item.trim());
  }
  return typeof value === 'string' && value.trim() ? [value.trim()] : [];
}

function collectProviderProbeFiles(providerPath, maxFiles = 200) {
  const files = [];
  const queue = [{ filePath: providerPath, depth: 0 }];
  while (queue.length > 0 && files.length < maxFiles) {
    const { filePath, depth } = queue.shift();
    let stat;
    try {
      stat = fs.statSync(filePath);
    } catch {
      continue;
    }
    if (stat.isFile()) {
      if (/\.(?:php|json|js|mjs|cjs|md|txt)$/i.test(filePath)) {
        files.push(filePath);
      }
      continue;
    }
    if (!stat.isDirectory() || depth >= 4) {
      continue;
    }
    for (const entry of fs.readdirSync(filePath, { withFileTypes: true })) {
      if (entry.name === 'vendor' || entry.name === 'node_modules' || entry.name === '.git') {
        continue;
      }
      queue.push({ filePath: path.join(filePath, entry.name), depth: depth + 1 });
    }
  }
  return files;
}

function codexProviderPluginInspection(providerPath) {
  const slug = path.basename(providerPath).toLowerCase();
  if (slug === 'ai-provider-for-opencode' || slug.includes('opencode')) {
    return { status: 'invalid', reason: 'opencode_provider_plugin' };
  }
  if (!fs.existsSync(providerPath)) {
    return { status: 'unknown', reason: 'path_not_available_on_parent' };
  }
  for (const filePath of collectProviderProbeFiles(providerPath)) {
    let contents = '';
    try {
      contents = fs.readFileSync(filePath, 'utf8');
    } catch {
      continue;
    }
    if (/codex/i.test(contents)) {
      return { status: 'valid', reason: 'codex_marker_found', marker_path: filePath };
    }
  }
  return { status: 'invalid', reason: 'no_codex_marker_found' };
}

function codexProviderPluginPreflightPayload(request, taskInput) {
  const explicitProvider = request.executor?.config?.provider;
  if (explicitProvider !== 'codex' || taskInput?.provider !== 'codex') {
    return null;
  }

  const providerPluginPaths = normalizeStringArray(taskInput.provider_plugin_paths);
  const inspections = providerPluginPaths.map((providerPath) => ({
    path: providerPath,
    ...codexProviderPluginInspection(providerPath),
  }));
  const invalidInspections = inspections.filter((inspection) => inspection.status === 'invalid');
  if (providerPluginPaths.length > 0 && invalidInspections.length === 0) {
    return null;
  }

  const message = providerPluginPaths.length === 0
    ? `WP Codebox Codex task has no provider_plugin_paths configured. ${CODEX_PROVIDER_PLUGIN_GUIDANCE}`
    : `WP Codebox Codex task selected provider plugin path(s) that do not look Codex-capable. ${CODEX_PROVIDER_PLUGIN_GUIDANCE}`;
  return {
    success: false,
    status: 'failed',
    failure_classification: 'provider',
    summary: message,
    diagnostics: [{
      class: 'codebox.preflight.codex_provider_plugin_path',
      message,
      data: {
        phase: 'codebox.preflight',
        provider: taskInput.provider,
        provider_plugin_paths: providerPluginPaths,
        inspections,
        expected: 'Codex-capable ai-provider-for-openai checkout from the Codex provider branch/PR.',
        guidance: CODEX_PROVIDER_PLUGIN_GUIDANCE,
      },
    }],
    metadata: {
      phase: 'codebox.preflight',
      provider: taskInput.provider,
      provider_plugin_paths: providerPluginPaths,
      inspections,
      codex_provider_plugin_required: true,
    },
  };
}

function missingModelPreflightPayload(taskInput) {
  if (!taskInput?.provider || taskInput?.model) {
    return null;
  }

  const message = `WP Codebox agent task requires an AI model for provider "${taskInput.provider}". Pass --model or set provider-config.model.`;
  return {
    success: false,
    status: 'failed',
    failure_classification: 'provider',
    summary: message,
    diagnostics: [{
      class: 'codebox.preflight.missing_model',
      message,
      data: {
        phase: 'codebox.preflight',
        provider: taskInput.provider,
        model_sources: ['--model', 'provider-config.model'],
      },
    }],
    metadata: {
      phase: 'codebox.preflight',
      provider: taskInput.provider,
      model_required: true,
    },
  };
}

async function loadCodeboxCoreNormalizers() {
  const configuredModule = process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE;
  const candidates = configuredModule ? [configuredModule] : [DEFAULT_CODEBOX_CORE_MODULE];

  for (const candidate of candidates) {
    try {
      const module = await importModule(candidate);
      if (typeof module.normalizeAgentTaskRunResult === 'function' || typeof module.normalizeRecipeRunSummary === 'function') {
        return {
          normalizeAgentTaskRunResult: typeof module.normalizeAgentTaskRunResult === 'function' ? module.normalizeAgentTaskRunResult : null,
          normalizeRecipeRunSummary: typeof module.normalizeRecipeRunSummary === 'function' ? module.normalizeRecipeRunSummary : null,
        };
      }
    } catch {
      // Fall back to the local compatibility path when Codebox core is not installed yet.
    }
  }

  return {};
}

function importModule(specifier) {
  if (specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('file:')) {
    return import(specifier.startsWith('file:') ? specifier : pathToFileURL(path.resolve(specifier)).href);
  }
  return import(specifier);
}

function runtimeOverlayConfigFailurePayload(error) {
  if (!Array.isArray(error?.diagnostics) || error.diagnostics.length === 0) {
    return null;
  }
  return {
    success: false,
    status: 'failed',
    summary: error.message || 'Invalid WordPress executor runtime overlay config.',
    failure_classification: 'provider',
    diagnostics: error.diagnostics,
    metadata: {
      phase: 'homeboy.wordpress.runtime_overlay_validation',
    },
  };
}

async function runTaskRunner(request) {
  const coreNormalizers = await loadCodeboxCoreNormalizers();
  const runner = argValue('--task-runner') || `${__dirname}/homeboy-wp-codebox-task-runner.cjs`;
  const config = request.executor?.config || {};
  let taskInput;
  try {
    taskInput = codeboxTaskRequestFromAgentTaskRequest(request);
  } catch (error) {
    const validationPayload = runtimeOverlayConfigFailurePayload(error);
    if (validationPayload) {
      return agentTaskOutcomeFromCodeboxResult(request, validationPayload, { exitStatus: 1, ...coreNormalizers });
    }
    throw error;
  }
  const missingModelPayload = missingModelPreflightPayload(taskInput);
  if (missingModelPayload) {
    return agentTaskOutcomeFromCodeboxResult(request, missingModelPayload, { exitStatus: 1, ...coreNormalizers });
  }
  const codexProviderPluginPayload = codexProviderPluginPreflightPayload(request, taskInput);
  if (codexProviderPluginPayload) {
    return agentTaskOutcomeFromCodeboxResult(request, codexProviderPluginPayload, { exitStatus: 1, ...coreNormalizers });
  }
  const preflightPayload = missingWorkspacePayload(request, taskInput);
  if (preflightPayload) {
    return agentTaskOutcomeFromCodeboxResult(request, preflightPayload, { exitStatus: 1, ...coreNormalizers });
  }
  const configArgs = [
    ['--agents-api', config.agents_api_path || config.agentsApiPath],
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
    input: JSON.stringify(taskInput),
    env: { ...process.env, ...codexAuthEnv(taskInput) },
    maxBuffer: 1024 * 1024 * 20,
    timeout: requestTimeoutMs(request),
  });

  if (result.stderr) {
    process.stderr.write(result.stderr);
  }

  let payload = {};
  if (result.error && result.error.code === 'ETIMEDOUT') {
    payload = timeoutPayload(requestTimeoutMs(request), request);
    return agentTaskOutcomeFromCodeboxResult(request, payload, { exitStatus: 1, ...coreNormalizers });
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

  return agentTaskOutcomeFromCodeboxResult(request, payload, { exitStatus: result.status ?? 1, ...coreNormalizers });
}

(async () => {
  try {
    if (hasFlag('--print-contract')) {
      process.stdout.write(`${JSON.stringify(providerContract(), null, 2)}\n`);
      process.exit(0);
    }

    const request = readRequest();
    const outcome = await runTaskRunner(request);
    process.stdout.write(`${JSON.stringify(outcome, null, 2)}\n`);
    process.exitCode = outcome.status === 'succeeded' || outcome.status === 'no_op' ? 0 : 1;
  } catch (error) {
    console.error(error && error.message ? error.message : String(error));
    process.exitCode = 1;
  }
})();
