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
const {
  normalizeStringArray,
  providerAuthEnvSources,
  providerDiagnosticClass,
  providerLabel,
  providerPluginValidation,
  providerSecretEnv,
} = require('../../lib/provider-preflight-manifest');
const {
  wpCodeboxRuntimeReadinessDiagnostics,
} = require('../../lib/wp-codebox-runtime-readiness');
const {
  loadWpCodeboxCore,
} = requireWpCodeboxCoreLoader();

const WP_CODEBOX_RUN_RESULTS_MODULE_OPTIONS = {
  packageCandidates: [
    '@automattic/wp-codebox-core/run-results',
    'wp-codebox-workspace/run-results',
    // Compatibility fallback for WP Codebox builds before focused package entrypoints.
    '@automattic/wp-codebox-core',
  ],
  packageDistEntries: ['run-results.js', 'index.js'],
};

function requireWpCodeboxCoreLoader() {
  const candidates = [
    path.resolve(__dirname, '../../../../wordpress/lib/wp-codebox-core-loader'),
    path.resolve(__dirname, '../../../../extensions/wordpress/lib/wp-codebox-core-loader'),
  ];

  for (const candidate of candidates) {
    try {
      return require(candidate);
    } catch (error) {
      if (!error || error.code !== 'MODULE_NOT_FOUND') {
        throw error;
      }
    }
  }

  throw new Error(`Unable to resolve wp-codebox-core-loader from ${candidates.join(', ')}`);
}

const DEFAULT_TASK_RUNNER = path.resolve(__dirname, 'homeboy-wp-codebox-task-runner.cjs');
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

function defaultAuthPath(source) {
  if (source?.path_env && process.env[source.path_env]) {
    return process.env[source.path_env];
  }
  if (source?.path === '~/.codex/auth.json') {
    return defaultCodexAuthPath();
  }
  if (typeof source?.path === 'string' && source.path.startsWith('~/') && process.env.HOME) {
    return path.join(process.env.HOME, source.path.slice(2));
  }
  return source?.path || '';
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

function nestedField(value, field) {
  return String(field || '').split('.').reduce((current, key) => {
    if (!current || typeof current !== 'object') {
      return undefined;
    }
    return current[key];
  }, value);
}

function sourceValue(source, auth) {
  for (const field of [source.field, ...normalizeStringArray(source.fallback_fields)]) {
    const value = nestedField(auth, field);
    if (value !== undefined && value !== null && value !== '') {
      return value;
    }
  }
  return source.value;
}

function providerAuthEnv(taskInput) {
  const provider = taskInput?.provider || '';
  const secretEnv = providerSecretEnv(provider);
  const sources = providerAuthEnvSources(provider);
  if (secretEnv.length === 0 || Object.keys(sources).length === 0) {
    return {};
  }
  if (secretEnv.every((name) => Boolean(process.env[name]))) {
    return {};
  }

  const authByPath = new Map();
  const env = {};
  for (const [name, source] of Object.entries(sources)) {
    if (!secretEnv.includes(name)) {
      continue;
    }
    const authPath = defaultAuthPath(source);
    if (!authPath) {
      continue;
    }
    if (!authByPath.has(authPath)) {
      authByPath.set(authPath, readJsonIfAvailable(authPath));
    }
    const auth = authByPath.get(authPath);
    if (source.source === 'json-file-jwt-expiration') {
      const fallbackValue = normalizeStringArray(source.fallback_fields)
        .map((field) => nestedField(auth, field))
        .find((value) => value !== undefined && value !== null && value !== '');
      if (fallbackValue !== undefined) {
        env[name] = String(fallbackValue);
        continue;
      }
      const token = nestedField(auth, source.field);
      env[name] = jwtExpiration(token);
      continue;
    }
    const value = sourceValue(source, auth);
    if (value !== undefined && value !== null && value !== '') {
      env[name] = String(value);
    }
  }
  const required = secretEnv.filter((name) => name !== 'AI_PROVIDER_OPENAI_CODEX_FEDRAMP');
  if (!required.every((name) => Boolean(env[name] || process.env[name]))) {
    return {};
  }
  return env;
}

function timeoutPayload(timeoutMs, request = {}) {
  const artifacts = argValue('--artifacts') || request.executor?.config?.artifacts || '';
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
  const evidenceRefs = evidencePath && fs.existsSync(evidencePath) ? [{
    kind: 'codebox-task-runner-preflight',
    uri: evidencePath,
    label: 'WP Codebox task runner preflight evidence',
  }] : [];
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
      },
    }],
    metadata: {
      timeout_ms: timeoutMs,
      timeout_classification: 'provider_timeout',
      artifacts,
      evidence_path: evidencePath,
      artifact_ref_count: knownArtifacts.length,
    },
  };
}

function redactDiagnosticText(text) {
  return String(text || '')
    .replace(/((?:TOKEN|SECRET|PASSWORD|API[_-]?KEY|ACCESS[_-]?KEY|REFRESH[_-]?TOKEN)[A-Z0-9_-]*\s*=\s*)\S+/gi, '$1[redacted]')
    .slice(0, 8000);
}

function missingSecretEnvNames(stderr) {
  const match = String(stderr || '').match(/(?:Required WP Codebox secret environment variable missing|(?:Claude Code|Codex) provider auth preflight failed: missing required secret environment (?:mapping|value)):\s*([A-Z0-9_,\s]+)/i);
  if (!match) {
    return [];
  }
  return match[1]
    .split(',')
    .map((name) => name.trim())
    .filter((name) => /^[A-Z0-9_]+$/.test(name));
}

function preflightFailureClass(stderr, missingSecretEnv) {
  for (const provider of ['codex', 'claude-code']) {
    const label = providerLabel(provider).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    if (new RegExp(`${label} provider auth preflight failed:`, 'i').test(stderr)) {
      return providerDiagnosticClass(provider) || 'codebox.preflight.stderr';
    }
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
    ? `WP Codebox task runner preflight is missing required secret environment variables for ${diagnosticClass === 'codebox.preflight.claude_code_auth' ? 'Claude Code provider auth' : (diagnosticClass === 'codebox.preflight.codex_auth' ? 'Codex provider auth' : 'the runtime')}: ${missingSecretEnv.join(', ')}.`
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

function workspaceRequiredValue(value) {
  if (value === true) {
    return true;
  }
  if (typeof value === 'string') {
    return ['1', 'true', 'yes', 'required'].includes(value.trim().toLowerCase());
  }
  return false;
}

function requiresWorkspace(request, taskInput) {
  const config = request.executor?.config || {};
  const parentConfig = taskInput.parent_request?.executor?.config || {};
  const materialization = request.workspace?.materialization || {};
  return workspaceRequiredValue(config.workspace_required)
    || workspaceRequiredValue(config.workspaceRequired)
    || workspaceRequiredValue(parentConfig.workspace_required)
    || workspaceRequiredValue(parentConfig.workspaceRequired)
    || workspaceRequiredValue(materialization.required)
    || workspaceRequiredValue(materialization.workspace_required)
    || workspaceRequiredValue(materialization.workspaceRequired);
}

function workspaceMounts(taskInput) {
  return Array.isArray(taskInput.mounts) ? taskInput.mounts : [];
}

function workspaceMountKind(mount) {
  return mount?.metadata?.kind;
}

function hasWorkspaceMount(taskInput) {
  return workspaceMounts(taskInput).some((mount) => {
    if (!mount || typeof mount !== 'object') {
      return false;
    }
    if (workspaceMountKind(mount) === 'homeboy-runtime-workspace') {
      return Boolean(mount.source);
    }
    return Boolean(mount.source) && /^\/workspace(?:\/|$)/.test(String(mount.target || ''));
  });
}

function missingWorkspacePayload(request, taskInput) {
  if (!requiresWorkspace(request, taskInput) || hasWorkspaceMount(taskInput)) {
    return null;
  }

  const config = request.executor?.config || {};
  const message = 'WP Codebox task requires a mounted checkout/workspace, but none was provided.';
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
        workspace_required: true,
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
      workspace_required: true,
      workspace_root: config.workspace_root || config.workspaceRoot || request.workspace?.root || '',
      mounts_count: workspaceMounts(taskInput).length,
      workspaces_count: Array.isArray(taskInput.workspaces) ? taskInput.workspaces.length : 0,
      workspace_materialization: request.workspace?.materialization || {},
    },
  };
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

function providerPluginInspection(providerPath, validation) {
  const slug = path.basename(providerPath).toLowerCase();
  if (normalizeStringArray(validation.invalid_slug_patterns).some((pattern) => slug.includes(pattern.toLowerCase()))) {
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
    if (new RegExp(validation.marker_pattern || validation.markerPattern || 'codex', 'i').test(contents)) {
      return { status: 'valid', reason: 'codex_marker_found', marker_path: filePath };
    }
  }
  return { status: 'invalid', reason: 'no_codex_marker_found' };
}

function providerPluginPreflightPayload(request, taskInput) {
  const validation = providerPluginValidation(taskInput?.provider || '');
  if (!validation) {
    return null;
  }

  const providerPluginPaths = normalizeStringArray(taskInput.provider_plugin_paths);
  const inspections = providerPluginPaths.map((providerPath) => ({
    path: providerPath,
    ...providerPluginInspection(providerPath, validation),
  }));
  const invalidInspections = inspections.filter((inspection) => inspection.status === 'invalid');
  if (providerPluginPaths.length > 0 && invalidInspections.length === 0) {
    return null;
  }

  const message = providerPluginPaths.length === 0
    ? `WP Codebox ${providerLabel(taskInput.provider)} task has no provider_plugin_paths configured. ${validation.guidance}`
    : `WP Codebox ${providerLabel(taskInput.provider)} task selected provider plugin path(s) that do not look ${providerLabel(taskInput.provider)}-capable. ${validation.guidance}`;
  return {
    success: false,
    status: 'failed',
    failure_classification: 'provider',
    summary: message,
    diagnostics: [{
      class: validation.diagnostic_class || 'codebox.preflight.provider_plugin_path',
      message,
      data: {
        phase: 'codebox.preflight',
        provider: taskInput.provider,
        provider_plugin_paths: providerPluginPaths,
        inspections,
        expected: validation.expected || '',
        guidance: validation.guidance || '',
      },
    }],
    metadata: {
      phase: 'codebox.preflight',
      provider: taskInput.provider,
      provider_plugin_paths: providerPluginPaths,
      inspections,
      provider_plugin_required: true,
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
  const module = await loadWpCodeboxCore(WP_CODEBOX_RUN_RESULTS_MODULE_OPTIONS);
  return Object.fromEntries(Object.entries({
    normalizeAgentTaskRunResult: module?.normalizeAgentTaskRunResult,
    normalizeRecipeRunSummary: module?.normalizeRecipeRunSummary,
    normalizeRuntimeProfile: module?.normalizeRuntimeProfile,
    normalizeRuntimeProfilePayload: module?.normalizeRuntimeProfilePayload,
    normalizeTypedArtifactEntry: module?.normalizeTypedArtifactEntry,
    normalizeTypedArtifactDto: module?.normalizeTypedArtifactDto,
    normalizeTypedArtifactDTO: module?.normalizeTypedArtifactDTO,
    normalizeTypedArtifacts: module?.normalizeTypedArtifacts,
    normalizeTypedArtifactMap: module?.normalizeTypedArtifactMap,
  }).filter(([, value]) => typeof value === 'function'));
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

function runtimeReadinessFailurePayload(taskInput) {
  const diagnostics = wpCodeboxRuntimeReadinessDiagnostics(taskInput);
  if (diagnostics.length === 0) {
    return null;
  }
  return {
    success: false,
    status: 'failed',
    failure_classification: 'provider',
    summary: diagnostics[0].message || 'WP Codebox runtime readiness preflight failed.',
    diagnostics,
    metadata: {
      phase: 'codebox.preflight',
      owner_surface: 'wp-codebox-runtime-integration',
      runtime_readiness_failed: true,
    },
  };
}

async function runTaskRunner(request) {
  const coreNormalizers = await loadCodeboxCoreNormalizers();
  const runner = argValue('--task-runner') || DEFAULT_TASK_RUNNER;
  const config = request.executor?.config || {};
  let taskInput;
  try {
    taskInput = codeboxTaskRequestFromAgentTaskRequest(request, coreNormalizers);
  } catch (error) {
    const validationPayload = runtimeOverlayConfigFailurePayload(error);
    if (validationPayload) {
      return agentTaskOutcomeFromCodeboxResult(request, validationPayload, { exitStatus: 1, ...coreNormalizers });
    }
    throw error;
  }
  const runtimeReadinessPayload = runtimeReadinessFailurePayload(taskInput);
  if (runtimeReadinessPayload) {
    return agentTaskOutcomeFromCodeboxResult(request, runtimeReadinessPayload, { exitStatus: 1, ...coreNormalizers });
  }
  const missingModelPayload = missingModelPreflightPayload(taskInput);
  if (missingModelPayload) {
    return agentTaskOutcomeFromCodeboxResult(request, missingModelPayload, { exitStatus: 1, ...coreNormalizers });
  }
  const providerPluginPayload = providerPluginPreflightPayload(request, taskInput);
  if (providerPluginPayload) {
    return agentTaskOutcomeFromCodeboxResult(request, providerPluginPayload, { exitStatus: 1, ...coreNormalizers });
  }
  const preflightPayload = missingWorkspacePayload(request, taskInput);
  if (preflightPayload) {
    return agentTaskOutcomeFromCodeboxResult(request, preflightPayload, { exitStatus: 1, ...coreNormalizers });
  }
  const configArgs = [
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
    env: { ...process.env, ...providerAuthEnv(taskInput) },
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

  return agentTaskOutcomeFromCodeboxResult(request, payload, { exitStatus: result.status ?? 1, ...coreNormalizers, ...legacyResultCompatibilityOptions(request, payload) });
}

function legacyResultCompatibilityOptions(request, payload = {}) {
  const config = request.executor?.config || {};
  const explicitConfig = config.allowLegacyCodeboxResultCompatibility || config.allow_legacy_codebox_result_compatibility;
  const explicitRunnerFallback = payload.metadata?.run_agent_task_compatibility?.legacy_result_normalization === true;
  return explicitConfig || explicitRunnerFallback ? { allowLegacyCodeboxResultCompatibility: true } : {};
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
