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
  if (!request || request.schema !== 'wp-codebox/task-input/v1') {
    throw new Error('Task request must use schema wp-codebox/task-input/v1.');
  }
  return request;
}

function secretEnvNames(request) {
  return Array.from(new Set([...(request.secret_env || []), ...(request.recipe?.secret_env || []), ...argValues('--secret-env')].filter(Boolean)));
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

function firstExistingPath(...candidates) {
  return candidates.find((candidate) => candidate && fs.existsSync(candidate)) || '';
}

function siblingPath(workspaceRoot, sibling) {
  return workspaceRoot ? path.join(path.dirname(workspaceRoot), sibling) : '';
}

function workspaceRootFromMounts(mounts) {
  const mountedWorkspace = mounts.find((mount) => mount?.metadata?.kind === 'homeboy-dmc-workspace') || mounts[0];
  return mountedWorkspace?.source || '';
}

function bundledAgentsApiPath(dataMachinePath) {
  return firstExistingPath(
    path.join(dataMachinePath || '', 'vendor', 'wordpress', 'agents-api'),
    path.join(dataMachinePath || '', 'vendor', 'automattic', 'agents-api'),
  );
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

function secretEnvValues(secretNames) {
  return Object.fromEntries(secretNames
    .filter((name) => typeof name === 'string' && name !== '' && process.env[name])
    .map((name) => [name, process.env[name]]));
}

function redactString(value, secrets) {
  return Object.entries(secrets).reduce((redacted, [name, secret]) => {
    if (!secret) {
      return redacted;
    }
    return redacted.split(secret).join(`[REDACTED:${name}]`);
  }, String(value));
}

function redactedValue(value, secrets) {
  if (typeof value === 'string') {
    return redactString(value, secrets);
  }
  if (Array.isArray(value)) {
    return value.map((item) => redactedValue(item, secrets));
  }
  if (plainObject(value)) {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => {
      if (/secret|token|password|credential|api[_-]?key/i.test(key)) {
        return [key, item ? '[REDACTED]' : item];
      }
      return [key, redactedValue(item, secrets)];
    }));
  }
  return value;
}

function writeEvidenceFile(artifacts, fileName, contents) {
  try {
    fs.mkdirSync(artifacts, { recursive: true });
    const filePath = path.join(artifacts, fileName);
    fs.writeFileSync(filePath, contents);
    return filePath;
  } catch {
    return '';
  }
}

function readJsonIfAvailable(filePath) {
  try {
    return JSON.parse(fs.readFileSync(filePath, 'utf8'));
  } catch {
    return null;
  }
}

function pathEvidenceCandidates(...texts) {
  const candidates = new Set();
  const pattern = /(?:[A-Za-z]:)?\/[^\s'"`]+(?:wp-codebox-agent-task-recipe|homeboy-wp-codebox-agent-task-input)[^\s'"`)]*/g;
  for (const text of texts) {
    for (const match of String(text || '').matchAll(pattern)) {
      candidates.add(match[0].replace(/[.,;:]+$/, ''));
    }
  }
  return [...candidates];
}

function copiedEvidencePathName(sourcePath, index) {
  const parent = path.basename(path.dirname(sourcePath)).replace(/[^A-Za-z0-9_.-]+/g, '-');
  const base = path.basename(sourcePath).replace(/[^A-Za-z0-9_.-]+/g, '-');
  return `captured-wp-codebox-path-${index + 1}-${parent}-${base}`;
}

function copyGeneratedEvidencePaths(artifacts, candidates, secrets) {
  const copied = [];
  candidates.forEach((candidate, index) => {
    try {
      if (!fs.existsSync(candidate) || !fs.statSync(candidate).isFile()) {
        return;
      }
      const parsed = candidate.endsWith('.json') ? readJsonIfAvailable(candidate) : null;
      const contents = parsed
        ? `${JSON.stringify(redactedValue(parsed, secrets), null, 2)}\n`
        : redactString(fs.readFileSync(candidate, 'utf8'), secrets);
      const target = writeEvidenceFile(artifacts, copiedEvidencePathName(candidate, index), contents);
      if (target) {
        copied.push({ source: candidate, path: target });
      }
    } catch {
      // Best-effort evidence capture must not mask the actual WP Codebox failure.
    }
  });
  return copied;
}

function preserveWpCodeboxFailureEvidence({ artifacts, inputPath, result, command, args, secretNames }) {
  const secrets = secretEnvValues(secretNames);
  const stdoutPath = result.stdout
    ? writeEvidenceFile(artifacts, 'wp-codebox-command-stdout.txt', redactString(result.stdout, secrets))
    : '';
  const stderrPath = result.stderr
    ? writeEvidenceFile(artifacts, 'wp-codebox-command-stderr.txt', redactString(result.stderr, secrets))
    : '';
  const stableInput = readJsonIfAvailable(inputPath);
  const inputEvidencePath = stableInput
    ? writeEvidenceFile(artifacts, 'wp-codebox-agent-task-input.redacted.json', `${JSON.stringify(redactedValue(stableInput, secrets), null, 2)}\n`)
    : '';
  const generatedPathCandidates = pathEvidenceCandidates(result.stdout, result.stderr);
  const copiedGeneratedPaths = copyGeneratedEvidencePaths(artifacts, generatedPathCandidates, secrets);
  const summary = {
    schema: 'homeboy/wp-codebox-command-evidence/v1',
    command,
    args,
    status: result.status,
    signal: result.signal,
    error: result.error ? result.error.message : '',
    input_path: inputPath,
    input_evidence_path: inputEvidencePath,
    stdout_path: stdoutPath,
    stderr_path: stderrPath,
    generated_path_candidates: generatedPathCandidates,
    copied_generated_paths: copiedGeneratedPaths,
  };
  const summaryPath = writeEvidenceFile(artifacts, 'wp-codebox-command-evidence.json', `${JSON.stringify(summary, null, 2)}\n`);
  return {
    ...summary,
    summary_path: summaryPath,
  };
}

function evidenceArtifacts(evidence) {
  return [
    { id: 'wp-codebox-command-evidence', kind: 'codebox-command-evidence', path: evidence.summary_path },
    { id: 'wp-codebox-agent-task-input', kind: 'codebox-agent-task-input', path: evidence.input_evidence_path },
    { id: 'wp-codebox-command-stdout', kind: 'codebox-command-log', path: evidence.stdout_path },
    { id: 'wp-codebox-command-stderr', kind: 'codebox-command-log', path: evidence.stderr_path },
    ...evidence.copied_generated_paths.map((item, index) => ({
      id: `wp-codebox-generated-evidence-${index + 1}`,
      kind: 'codebox-generated-input',
      path: item.path,
      metadata: { source: item.source },
    })),
  ].filter((artifact) => artifact.path);
}

function attachFailureEvidence(payload, evidence) {
  const artifacts = evidenceArtifacts(evidence);
  const diagnostics = evidence.summary_path ? [{
    class: 'wp-codebox.command.evidence_preserved',
    message: 'WP Codebox command stdout, stderr, and redacted task input were preserved for failure diagnosis.',
    data: {
      evidence_path: evidence.summary_path,
      stdout_path: evidence.stdout_path,
      stderr_path: evidence.stderr_path,
      input_evidence_path: evidence.input_evidence_path,
      generated_path_candidates: evidence.generated_path_candidates,
      copied_generated_paths: evidence.copied_generated_paths,
    },
  }] : [];
  return {
    ...payload,
    artifacts: Array.isArray(payload.artifacts) ? [...payload.artifacts, ...artifacts] : payload.artifacts,
    evidence_refs: [
      ...(Array.isArray(payload.evidence_refs) ? payload.evidence_refs : []),
      ...artifacts.map((artifact) => ({ kind: artifact.kind, uri: artifact.path, label: artifact.kind.replace(/-/g, ' ') })),
    ],
    diagnostics: [...(Array.isArray(payload.diagnostics) ? payload.diagnostics : []), ...diagnostics],
    metadata: {
      ...(plainObject(payload.metadata) ? payload.metadata : {}),
      wp_codebox_command_evidence: evidence.summary_path,
    },
  };
}

const LEGACY_RUNTIME_PREFIX = ['data', 'machine'].join('_');
const WP_CODEBOX_RUNTIME_PATH_KEY = `${LEGACY_RUNTIME_PREFIX}_path`;
const WP_CODEBOX_RUNTIME_TOOLS_PATH_KEY = `${LEGACY_RUNTIME_PREFIX}_code_path`;
const LEGACY_BUNDLE_KEYS = [
  `${LEGACY_RUNTIME_PREFIX}_bundle`,
  `${LEGACY_RUNTIME_PREFIX}Bundle`,
];

function legacyValue(source, suffix = '') {
  if (!source || typeof source !== 'object') {
    return '';
  }
  const key = suffix ? `${LEGACY_RUNTIME_PREFIX}_${suffix}` : LEGACY_RUNTIME_PREFIX;
  return source[key] || source[`${key}_path`] || '';
}

function requestAgentBundle(request) {
  if (request.agent_bundle && typeof request.agent_bundle === 'object') {
    return request.agent_bundle;
  }
  for (const key of LEGACY_BUNDLE_KEYS) {
    if (request[key] && typeof request[key] === 'object') {
      return request[key];
    }
  }
  return {};
}

function requestRuntimeComponents(request, mounts = []) {
  const explicit = request.runtime_component_paths && typeof request.runtime_component_paths === 'object'
    ? request.runtime_component_paths
    : {};
  const workspaceRoot = workspaceRootFromMounts(mounts);
  const dataMachinePath = explicit.agent_runtime || legacyValue(request) || firstExistingPath(siblingPath(workspaceRoot, 'data-machine'));
  return Object.fromEntries(Object.entries({
    ...explicit,
    agents_api: explicit.agents_api || request.agents_api_path || request.agents_api || bundledAgentsApiPath(dataMachinePath),
    agent_runtime: dataMachinePath,
    agent_runtime_tools: explicit.agent_runtime_tools || legacyValue(request, 'code') || firstExistingPath(siblingPath(workspaceRoot, 'data-machine-code')),
  }).filter(([, value]) => value !== '' && value !== undefined));
}

function runnerInput(request, artifacts) {
  const mounts = mountEntries(request);
  const runtimeComponentPaths = requestRuntimeComponents(request, mounts);
  return Object.fromEntries(Object.entries({
    parent_request: request,
    agent: argValue('--agent') || request.agent || 'wp-codebox-sandbox',
    mode: argValue('--mode') || request.mode || 'sandbox',
    provider: argValue('--provider') || request.provider || '',
    model: argValue('--model') || request.model || '',
    provider_plugin_paths: [...(request.provider_plugin_paths || []), ...argValues('--provider-plugin-path')],
    runtime_overlay_profiles: request.runtime_overlay_profiles || request.runtimeOverlayProfiles || [],
    secret_env: secretEnvNames(request),
    mounts,
    runtime_stack_mounts: runtimeStackMountEntries(request),
    runtime_overlays: runtimeOverlayEntries(request),
    max_turns: Number.parseInt(argValue('--max-turns') || request.max_turns || request.maxTurns || 0, 10) || undefined,
    task_timeout_seconds: Number.parseInt(argValue('--task-timeout-seconds') || request.task_timeout_seconds || request.taskTimeoutSeconds || 0, 10) || undefined,
    sandbox_session_id: request.sandbox_session_id || '',
    orchestrator: request.orchestrator || {},
    recipe: request.recipe || {},
    runtime_task: request.runtime_task || request.runtimeTask,
    artifacts_path: artifacts,
    wp_codebox_bin: argValue('--wp-codebox-bin') || request.wp_codebox_bin || '',
    agents_api_path: argValue('--agents-api') || request.agents_api_path || request.agents_api || '',
    runtime_component_paths: runtimeComponentPaths,
    homeboy_path: argValue('--homeboy') || request.homeboy_path || request.homeboy || '',
    homeboy_extensions_path: argValue('--homeboy-extensions') || request.homeboy_extensions_path || request.homeboy_extensions || path.resolve(__dirname, '..', '..'),
    wp_version: request.wp_codebox_wordpress_version || request.wp_version || request.wp || undefined,
    agent_bundle: requestAgentBundle(request),
  }).filter(([, value]) => value !== '' && value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function runtimeComponentExtraPlugins(input) {
  const components = {
    agents_api: input.agents_api_path,
    ...(input.runtime_component_paths || {}),
  };
  return [
    { key: 'agents_api', slug: 'agents-api' },
    { key: 'agent_runtime', slug: 'data-machine' },
    { key: 'agent_runtime_tools', slug: 'data-machine-code' },
  ].flatMap(({ key, slug }) => {
    const source = components[key];
    return source ? [{ source, slug, loadAs: 'mu-plugin', activate: false }] : [];
  });
}

function componentContracts(input) {
  // Translate the runtime component paths (agents-api, data-machine,
  // data-machine-code) into the WP Codebox 0.8.0 `component_contracts` shape:
  // `{ slug, path, loadAs, activate }`. WP Codebox mounts these as mu-plugins so
  // Data Machine loads its own vendored Agents API and registers agents/chat.
  return runtimeComponentExtraPlugins(input).map((plugin) => ({
    slug: plugin.slug,
    path: plugin.source,
    loadAs: plugin.loadAs || 'mu-plugin',
    activate: Boolean(plugin.activate),
  }));
}

function verifySteps(input) {
  // Verification gates can come from the request directly or from the parent
  // request/task. Each entry is a WP Codebox recipe step (e.g.
  // `{ command: 'wordpress.phpunit', args: ['plugin-slug=data-machine'] }`)
  // that runs after the agent finishes; a non-zero exit fails the run.
  const candidates = [
    input.verify_steps,
    input.parent_request?.verify_steps,
    input.parent_request?.task?.verify_steps,
  ];
  for (const candidate of candidates) {
    if (Array.isArray(candidate) && candidate.length > 0) {
      return candidate.filter((step) => step && typeof step === 'object' && typeof step.command === 'string' && step.command !== '');
    }
  }
  return [];
}

function extraPlugins(input) {
  const explicit = input.parent_request?.extra_plugins || input.parent_request?.extraPlugins || [];
  const plugins = [...runtimeComponentExtraPlugins(input), ...(Array.isArray(explicit) ? explicit : [])];
  const seen = new Set();
  return plugins.filter((plugin) => {
    const key = `${plugin.slug || ''}:${plugin.source || ''}`;
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
}

function stableTaskInput(input) {
  const allowedTools = input.parent_request?.allowed_tools || input.parent_request?.task?.allowed_tools || [];
  const secretEnv = input.secret_env || [];
  return Object.fromEntries(Object.entries({
    schema: 'wp-codebox/task-input/v1',
    version: 1,
    goal: input.parent_request?.goal || input.parent_request?.task?.prompt || input.parent_request?.task?.goal || '',
    target: input.parent_request?.target || input.parent_request?.task?.target || {},
    allowed_tools: allowedTools,
    expected_artifacts: input.parent_request?.expected_artifacts || input.parent_request?.task?.expected_artifacts || [],
    sandbox_tool_policy: sandboxToolPolicy(input, allowedTools),
    policy: input.parent_request?.policy || input.parent_request?.task?.policy || {},
    context: input.parent_request?.context || input.parent_request?.task?.context || {},
    recipe: input.recipe || input.parent_request?.recipe || {},
    provider: input.provider,
    model: input.model,
    provider_plugin_paths: input.provider_plugin_paths || [],
    extra_plugins: extraPlugins(input),
    // WP Codebox 0.8.0 reads runtime component plugins (agents-api, data-machine,
    // data-machine-code) from `component_contracts`, not the legacy
    // `runtime_component_paths` / `data_machine_path` fields. Without this the
    // components never mount and agents/chat is unavailable in the sandbox.
    component_contracts: componentContracts(input),
    // Post-agent verification gate. WP Codebox emits these as recipe
    // `workflow.after` steps that run once the agent finishes editing; any
    // non-zero exit fails the whole run, so the orchestrator cannot report
    // success until the supplied gates (e.g. the repo's smoke/test suite) pass.
    verify_steps: verifySteps(input),
    runtime_overlay_profiles: input.runtime_overlay_profiles || [],
    secret_env: secretEnv,
    mounts: input.mounts || [],
    workspaces: input.parent_request?.workspaces || [],
    runtime_stack_mounts: input.runtime_stack_mounts || [],
    runtime_overlays: input.runtime_overlays || [],
    max_turns: input.max_turns,
    task_timeout_seconds: input.task_timeout_seconds,
    sandbox_session_id: input.sandbox_session_id,
    session_id: input.parent_request?.session_id || '',
    group_key: input.parent_request?.group_key || input.parent_request?.context?.group_key || '',
    audit_findings: input.parent_request?.audit_findings || input.parent_request?.context?.audit_findings || [],
    orchestrator: input.orchestrator || {},
    artifacts_path: input.artifacts_path,
    wp_codebox_bin: input.wp_codebox_bin,
    agents_api_path: input.agents_api_path,
    [WP_CODEBOX_RUNTIME_PATH_KEY]: input.runtime_component_paths?.agent_runtime,
    [WP_CODEBOX_RUNTIME_TOOLS_PATH_KEY]: input.runtime_component_paths?.agent_runtime_tools,
    runtime_component_paths: input.runtime_component_paths || {},
    wp: input.wp_version,
    agent_bundle: isAgentBundle(input) ? agentBundleConfig(input, input.agent_bundle || {}) : {},
    runtime_task: runtimeTask(input),
    parent_request: input.parent_request,
  }).filter(([, value]) => value !== '' && value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function normalizeTypedArtifactEntry(name, artifact) {
  if (!plainObject(artifact)) {
    return null;
  }
  const artifactName = artifact.name || name;
  if (!artifactName) {
    return null;
  }
  const fileRefs = typedArtifactFileRefs(artifact);
  return Object.fromEntries(Object.entries({
    schema: 'homeboy/agent-task-typed-artifact/v1',
    name: artifactName,
    type: artifact.type || artifact.kind || artifact.artifact_type || artifact.artifactType,
    artifact_schema: artifact.artifact_schema || artifact.artifactSchema || artifact.schema,
    payload: artifact.payload !== undefined ? artifact.payload : artifact.data,
    provenance: plainObject(artifact.provenance) ? artifact.provenance : {},
    file_refs: fileRefs,
    metadata: plainObject(artifact.metadata) ? artifact.metadata : {},
  }).filter(([, value]) => value !== undefined && value !== '' && !(Array.isArray(value) && value.length === 0)));
}

function typedArtifactFileRefs(artifact) {
  if (Array.isArray(artifact.file_refs)) {
    return artifact.file_refs;
  }
  if (Array.isArray(artifact.fileRefs)) {
    return artifact.fileRefs;
  }
  return [];
}

function normalizeTypedArtifacts(value) {
  if (Array.isArray(value)) {
    return Object.fromEntries(value
      .map((artifact, index) => normalizeTypedArtifactEntry(artifact?.name || artifact?.id || `artifact_${index + 1}`, artifact))
      .filter(Boolean)
      .map((artifact) => [artifact.name, artifact]));
  }
  if (!plainObject(value)) {
    return {};
  }
  return Object.fromEntries(Object.entries(value)
    .map(([name, artifact]) => normalizeTypedArtifactEntry(name, artifact))
    .filter(Boolean)
    .map((artifact) => [artifact.name, artifact]));
}

function mergeTypedArtifactOutputs(outputs, ...candidates) {
  const typedArtifacts = Object.assign({}, ...candidates.map(normalizeTypedArtifacts));
  if (Object.keys(typedArtifacts).length === 0) {
    return outputs;
  }
  return {
    ...outputs,
    typed_artifacts: {
      ...(plainObject(outputs.typed_artifacts) ? outputs.typed_artifacts : {}),
      ...typedArtifacts,
    },
  };
}

function sandboxToolPolicy(input, allowedTools) {
  const explicit = input.parent_request?.sandbox_tool_policy
    || input.parent_request?.sandboxToolPolicy
    || input.parent_request?.task?.sandbox_tool_policy
    || input.parent_request?.task?.sandboxToolPolicy;
  if (plainObject(explicit) && Array.isArray(explicit.tools) && explicit.tools.length > 0) {
    return explicit;
  }

  const tools = Array.isArray(allowedTools) ? allowedTools.filter((tool) => typeof tool === 'string' && tool.trim() !== '') : [];
  return {
    schema: 'wp-codebox/sandbox-tool-policy/v1',
    version: 1,
    tools: tools.length > 0
      ? tools.map((tool) => {
          const id = tool.trim();
          return {
            id,
            runtime_tool_id: id.replace(/^datamachine\//, '').replace(/[^A-Za-z0-9_]+/g, '_'),
            execution_location: 'sandbox',
            transport_visibility: 'sandbox',
            allowed: true,
            runtime: { environment: 'runtime_local', capability_scope: 'runtime_local' },
            metadata: { source: 'homeboy_allowed_tools' },
          };
        })
      : [{
          id: 'homeboy/no-runtime-tools',
          runtime_tool_id: 'homeboy_no_runtime_tools',
          execution_location: 'external',
          transport_visibility: 'hidden',
          allowed: false,
          runtime: { environment: 'control_plane', capability_scope: 'control_plane' },
          metadata: { source: 'homeboy_default_empty_policy' },
        }],
    metadata: { source: 'homeboy-wp-codebox-task-runner' },
  };
}

function isAgentBundle(input) {
  return Boolean(input.agent_bundle && Object.keys(input.agent_bundle).length > 0);
}

function runtimeTask(input) {
  if (plainObject(input.runtime_task)) {
    return input.runtime_task;
  }
  if (isAgentBundle(input)) {
    return agentBundleRuntimeTask(input, input.agent_bundle || {});
  }
  return undefined;
}

function agentBundleConfig(input, bundleConfig = {}) {
  const runtimeComponentPaths = input.runtime_component_paths || {};
  return Object.fromEntries(Object.entries({
    ...bundleConfig,
    prompt: bundleConfig.prompt || input.parent_request?.task?.prompt || input.parent_request?.goal || '',
    provider: bundleConfig.provider || input.provider || '',
    model: bundleConfig.model || input.model || '',
    provider_plugin_paths: bundleConfig.provider_plugin_paths || input.provider_plugin_paths || [],
    wp_codebox_artifacts_dir: input.artifacts_path,
    wp_codebox_components: runtimeComponentPaths,
  }).filter(([, value]) => value !== '' && value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function agentBundleRuntimeTask(input, bundleConfig = {}) {
  const config = agentBundleConfig(input, bundleConfig);
  const source = config.source || config.bundle_path || config.bundle_host_path || '';
  const runtimeBundles = Array.isArray(config.runtime_bundles) ? config.runtime_bundles : [];
  return {
    ability: 'datamachine/run-agent-bundle',
    input: Object.fromEntries(Object.entries({
      ...config,
      source,
      wait_for_completion: config.wait_for_completion ?? true,
      runtime_bundles: runtimeBundles,
    }).filter(([, value]) => value !== '' && value !== undefined && !(Array.isArray(value) && value.length === 0))),
  };
}

function resultExecutions(result) {
  if (Array.isArray(result.executions) && result.executions.length > 0) {
    return result.executions;
  }
  if (Array.isArray(result.run?.executions)) {
    return result.run.executions;
  }
  return [];
}

function normalizeAgentTaskRun(input, result) {
  if (!isAgentBundle(input)) {
    return result;
  }

  const executions = resultExecutions(result);
  const execution = executions.find((item) => item?.recipeCommand === 'wp-codebox.agent-sandbox-run') || executions[0] || null;
  const agentBundle = agentBundleConfig(input, input.agent_bundle || {});
  const stdoutWorkload = agentRuntimeWorkloadFromExecutionStdout(execution, agentBundle);
  const fallbackAgentResult = result.metadata?.agent_runtime?.workload || execution?.agentResult || result.run?.agentResult || result.agentResult || result.agent_result || {};
  const agentResult = hasSemanticWorkload(stdoutWorkload) ? stdoutWorkload : fallbackAgentResult;
  const bundleValidation = validateAgentRuntimeWorkload(agentResult, agentBundle);
  const success = !bundleValidation;
  const diagnostics = [
    ...(result.diagnostics || []),
    ...(bundleValidation ? [bundleValidation] : []),
    ...(agentRuntimeDiagnostics(agentResult) || []),
  ];

  return {
    ...result,
    success,
    status: success ? 'completed' : result.status,
    outputs: plainObject(agentResult.outputs) ? agentResult.outputs : result.outputs,
    summary: success ? 'WP Codebox agent task succeeded.' : (bundleValidation?.message || result.summary || 'WP Codebox agent task failed.'),
    session: result.session ? {
      ...result.session,
      status: success ? 'completed' : 'failed',
    } : result.session,
    run: {
      ...(result.run || {}),
      agentResult,
    },
    diagnostics,
    metadata: {
      ...(result.metadata || {}),
      agent_runtime: {
        ...(result.metadata?.agent_runtime || {}),
        bundle: agentBundle,
        workload: agentResult,
      },
    },
  };
}

function parseJsonObject(value) {
  if (!value || typeof value !== 'string') {
    return null;
  }
  try {
    const parsed = JSON.parse(value);
    return parsed && typeof parsed === 'object' ? parsed : null;
  } catch {
    return null;
  }
}

function agentRuntimeWorkloadFromExecutionStdout(execution, config) {
  const wrapper = parseJsonObject(execution?.stdout || '');
  const workload = parseJsonObject(wrapper?.output || '') || parseJsonObject(execution?.stdout || '');
  if (!workload) {
    return null;
  }
  const bundleRun = workload.agent_runtime?.result || workload.result || workload;
  if (bundleRun?.schema && String(bundleRun.schema).endsWith('/agent-bundle-run/v1')) {
    return agentRuntimeWorkloadFromBundleRun(bundleRun, config);
  }
  if (Array.isArray(workload.scenarios)) {
    return workload;
  }
  if (isSingleResultWorkload(workload)) {
    return agentRuntimeWorkloadFromSingleResult(workload, config);
  }
  if (workload.metadata || workload.metrics) {
    return {
      scenarios: [{
        id: config.workload_id || config.agent_slug || config.flow_slug || 'agent-bundle',
        metrics: workload.metrics || {},
        metadata: workload.metadata || {},
      }],
    };
  }
  return null;
}

function isSingleResultWorkload(workload) {
  return plainObject(workload) && (
    plainObject(workload.outputs)
      || plainObject(workload.output)
      || Array.isArray(workload.diagnostics)
      || typeof workload.summary === 'string'
  );
}

function agentRuntimeWorkloadFromSingleResult(workload, config) {
  let outputs = {};
  if (plainObject(workload.outputs)) {
    outputs = workload.outputs;
  } else if (plainObject(workload.output)) {
    outputs = workload.output;
  }
  outputs = mergeTypedArtifactOutputs(outputs, workload.typed_artifacts, workload.typedArtifacts, outputs.typed_artifacts, outputs.typedArtifacts);
  return Object.fromEntries(Object.entries({
    id: config.workload_id || config.agent_slug || config.flow_slug || 'agent-bundle',
    success: workload.success,
    status: workload.status,
    summary: workload.summary || workload.message,
    outputs,
    diagnostics: Array.isArray(workload.diagnostics) ? workload.diagnostics : [],
    metrics: workload.metrics || {},
    metadata: workload.metadata || {},
  }).filter(([, value]) => value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function agentRuntimeWorkloadFromBundleRun(bundleRun, config) {
  const bundle = bundleRun.bundle && typeof bundleRun.bundle === 'object' ? bundleRun.bundle : {};
  const workflowSteps = Array.isArray(bundleRun.workflow?.steps) ? bundleRun.workflow.steps : [];
  const outputs = mergeTypedArtifactOutputs({
    ...(plainObject(bundleRun.outputs) ? bundleRun.outputs : {}),
    ...toolRecorderOutputs(bundleRun.engine_data, config),
  }, bundleRun.typed_artifacts, bundleRun.typedArtifacts, bundleRun.outputs?.typed_artifacts, bundleRun.outputs?.typedArtifacts);
  return {
    outputs,
    scenarios: [{
      id: config.workload_id || bundle.flow_slug || bundle.bundle_slug || config.agent_slug || config.flow_slug || 'agent-bundle',
      metrics: {
        workflow_step_count: workflowSteps.length,
      },
      metadata: {
        schema: bundleRun.schema,
        success: bundleRun.success !== false,
        dry_run: Boolean(bundleRun.dry_run),
        bundle,
        job_id: bundleRun.job_id,
        job_status: bundleRun.job_status,
        wait_result: bundleRun.wait_result,
        engine_data: bundleRun.engine_data,
        error: bundleRun.success === false ? bundleRun.error : undefined,
      },
    }],
  };
}

function hasScenarios(value) {
  return Array.isArray(value?.scenarios) && value.scenarios.length > 0;
}

function hasSemanticWorkload(value) {
  return hasScenarios(value) || plainObject(value?.outputs) || isSingleResultWorkload(value);
}

function pathValue(source, dottedPath) {
  return String(dottedPath || '').split('.').filter(Boolean).reduce((value, key) => (value && typeof value === 'object' ? value[key] : undefined), source);
}

function stepDataPackets(engineData) {
  const packetsByStep = plainObject(engineData?.direct_step_data_packets) ? engineData.direct_step_data_packets : {};
  return Object.values(packetsByStep).flatMap((packets) => (Array.isArray(packets) ? packets : []));
}

function toolRecorderOutputs(engineData, config) {
  if (!plainObject(engineData) || !Array.isArray(config.tool_recorders)) {
    return {};
  }

  const outputs = {};
  const packets = stepDataPackets(engineData);
  for (const recorder of config.tool_recorders) {
    if (!plainObject(recorder) || typeof recorder.tool !== 'string') {
      continue;
    }

    const record = plainObject(recorder.record) ? recorder.record : {};
    const fields = plainObject(record.fields) ? record.fields : {};
    if (Object.keys(fields).length === 0) {
      continue;
    }

    const packet = packets.find((candidate) => {
      const metadata = plainObject(candidate?.metadata) ? candidate.metadata : {};
      return metadata.tool_name === recorder.tool && metadata.step_execution_success === true;
    });
    if (!packet) {
      continue;
    }

    const metadata = plainObject(packet.metadata) ? packet.metadata : {};
    const sources = [
      metadata.tool_result_data,
      metadata.tool_result_envelope,
      metadata.tool_result_envelope?.result,
    ].filter(plainObject);

    for (const [outputName, resultPath] of Object.entries(fields)) {
      if (outputs[outputName] !== undefined || typeof resultPath !== 'string') {
        continue;
      }
      for (const source of sources) {
        const value = pathValue(source, resultPath);
        if (value !== undefined && value !== null && value !== '') {
          outputs[outputName] = value;
          break;
        }
      }
    }
  }

  return outputs;
}

function validateAgentRuntimeWorkload(workload, config) {
  const scenarios = Array.isArray(workload?.scenarios) ? workload.scenarios : [];
  const failedScenario = scenarios.find((scenario) => scenario?.metadata?.error || scenario?.metadata?.error_message);
  if (failedScenario) {
    return {
      class: 'agent_runtime.workload.failed',
      message: failedScenario.metadata.error || failedScenario.metadata.error_message,
      data: { reason: 'scenario_error', scenario_id: failedScenario.id, metadata: failedScenario.metadata },
    };
  }

  const outputs = config.engine_data_outputs && typeof config.engine_data_outputs === 'object' ? config.engine_data_outputs : {};
  const missing = [];
  for (const [name, outputPath] of Object.entries(outputs)) {
    if (workload?.outputs?.[name] !== undefined && workload.outputs[name] !== null && workload.outputs[name] !== '') {
      continue;
    }
    const present = scenarios.some((scenario) => {
      const value = pathValue(scenario, outputPath);
      return value !== undefined && value !== null && value !== '';
    });
    if (!present) {
      missing.push({ name, path: outputPath });
    }
  }

  if (missing.length > 0) {
    return {
      class: 'agent_runtime.workload.incomplete',
      message: `Agent bundle workload did not produce required semantic outputs: ${missing.map((item) => item.name).join(', ')}.`,
      data: { reason: 'missing_engine_data_outputs', missing },
    };
  }

  if (scenarios.length === 0 && (!plainObject(workload?.outputs) || Object.keys(workload.outputs).length === 0)) {
    return {
      class: 'agent_runtime.workload.incomplete',
      message: 'Agent bundle workload did not produce scenarios or semantic outputs.',
      data: { reason: 'missing_semantic_outputs' },
    };
  }

  return null;
}

function agentRuntimeDiagnostics(workload) {
  const workloadDiagnostics = Array.isArray(workload?.diagnostics) ? workload.diagnostics.map((diagnostic) => ({
    class: diagnostic.class || diagnostic.kind || 'agent_runtime.workload',
    message: diagnostic.message || String(diagnostic),
    data: diagnostic.data || {},
  })) : [];
  const scenarios = Array.isArray(workload?.scenarios) ? workload.scenarios : [];
  const diagnostics = scenarios
    .filter((scenario) => scenario?.metadata?.error || scenario?.metadata?.error_message)
    .map((scenario) => ({
      class: 'agent_runtime.workload',
      message: scenario.metadata.error || scenario.metadata.error_message,
      data: { scenario_id: scenario.id, metadata: scenario.metadata },
    }));
  const allDiagnostics = [...workloadDiagnostics, ...diagnostics];
  return allDiagnostics.length > 0 ? allDiagnostics : null;
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
    summary: `WP Codebox agent-task-run timed out after ${timeoutMs}ms.`,
    artifacts: artifact,
    evidence_refs: evidencePath ? [{
      kind: 'codebox-task-runner-preflight',
      uri: evidencePath,
      label: 'WP Codebox task runner preflight evidence',
    }] : [],
    diagnostics: [{
      class: 'codebox.run_agent_task.timeout',
      message: 'wp-codebox agent-task-run exceeded the configured task timeout.',
      data: { timeout_ms: timeoutMs, inputPath, artifacts },
    }],
    metadata: { timeout_ms: timeoutMs, inputPath, artifacts, command, args },
  };
}

function commandFailurePayload(result, artifacts, evidence) {
  const message = result.stderr || result.stdout || result.error?.message || 'WP Codebox agent-task-run failed.';
  return attachFailureEvidence({
    success: false,
    status: 'failed',
    summary: message.split('\n').find((line) => line.trim() !== '') || 'WP Codebox agent-task-run failed.',
    artifacts: [],
    evidence_refs: [],
    diagnostics: [{
      class: 'wp-codebox.agent_task_run_failed',
      message: message.trim() || 'WP Codebox agent-task-run failed.',
      data: {
        status: result.status,
        signal: result.signal,
        error: result.error ? result.error.message : '',
        artifacts,
      },
    }],
    metadata: {
      status: result.status,
      signal: result.signal,
      error: result.error ? result.error.message : '',
    },
  }, evidence);
}

function runWpCodeboxParentTask(request) {
  const explicitArtifacts = argValue('--artifacts') || request.artifacts_path || '';
  const artifacts = explicitArtifacts || fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-artifacts-'));
  if (explicitArtifacts) {
    const riskyMount = mountEntries(request).find((mount) => pathInside(mount.source, explicitArtifacts));
    if (riskyMount) {
      console.error(`Warning: WP Codebox artifact directory is inside mounted source ${riskyMount.source} and may be captured recursively: ${explicitArtifacts}`);
    }
  }

  const input = runnerInput(request, artifacts);
  const inputPath = writeJsonFile('homeboy-wp-codebox-agent-task-input-', stableTaskInput(input));
  const wpCodeboxBin = input.wp_codebox_bin || process.env.HOMEBOY_WP_CODEBOX_BIN || 'wp-codebox';
  const args = ['agent-task-run', `--input-file=${inputPath}`, '--json'];
  const previewHold = argValue('--preview-hold');
  if (previewHold) {
    args.push(`--preview-hold-seconds=${previewHold}`);
  }
  const previewPublicUrl = argValue('--preview-public-url');
  if (previewPublicUrl) {
    args.push(`--preview-public-url=${previewPublicUrl}`);
  }

  const resolved = resolveCommand(wpCodeboxBin, args);
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
    env: {
      ...process.env,
    },
    maxBuffer: 1024 * 1024 * 20,
    timeout: timeoutMs,
  });
  const shouldPreserveEvidence = Boolean(result.error) || result.status !== 0;
  const failureEvidence = shouldPreserveEvidence ? preserveWpCodeboxFailureEvidence({
    artifacts,
    inputPath,
    result,
    command: resolved.command,
    args: resolved.args,
    secretNames: input.secret_env || [],
  }) : null;

  if (result.error && result.error.code === 'ETIMEDOUT') {
    process.stdout.write(`${JSON.stringify(timeoutPayload(timeoutMs, artifacts, evidencePath, inputPath, resolved.command, resolved.args), null, 2)}\n`);
    return 1;
  }

  if (result.stdout) {
    try {
      const payload = normalizeAgentTaskRun(input, JSON.parse(result.stdout));
      const payloadFailed = payload.success === false || payload.status === 'failed' || payload.session?.status === 'failed';
      const payloadEvidence = failureEvidence || (payloadFailed ? preserveWpCodeboxFailureEvidence({
        artifacts,
        inputPath,
        result,
        command: resolved.command,
        args: resolved.args,
        secretNames: input.secret_env || [],
      }) : null);
      const enrichedPayload = payloadEvidence ? attachFailureEvidence(payload, payloadEvidence) : payload;
      process.stdout.write(`${JSON.stringify(enrichedPayload, null, 2)}\n`);
      return payload.success === false ? 1 : 0;
    } catch {
      if (failureEvidence) {
        process.stdout.write(`${JSON.stringify(commandFailurePayload(result, artifacts, failureEvidence), null, 2)}\n`);
        return result.status ?? 1;
      }
      process.stdout.write(result.stdout);
    }
  }
  if (result.stderr) {
    if (failureEvidence) {
      process.stdout.write(`${JSON.stringify(commandFailurePayload(result, artifacts, failureEvidence), null, 2)}\n`);
      return result.status ?? 1;
    }
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
