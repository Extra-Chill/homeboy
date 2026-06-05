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

function requestRuntimeComponents(request) {
  const explicit = request.runtime_component_paths && typeof request.runtime_component_paths === 'object'
    ? request.runtime_component_paths
    : {};
  return Object.fromEntries(Object.entries({
    ...explicit,
    agents_api: explicit.agents_api || request.agents_api_path || request.agents_api,
    agent_runtime: explicit.agent_runtime || legacyValue(request),
    agent_runtime_tools: explicit.agent_runtime_tools || legacyValue(request, 'code'),
  }).filter(([, value]) => value !== '' && value !== undefined));
}

function runnerInput(request, artifacts) {
  const runtimeComponentPaths = requestRuntimeComponents(request);
  return Object.fromEntries(Object.entries({
    parent_request: request,
    agent: argValue('--agent') || request.agent || 'wp-codebox-sandbox',
    mode: argValue('--mode') || request.mode || 'sandbox',
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
    recipe: request.recipe || {},
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
    runtime_task: isAgentBundle(input) ? agentBundleRuntimeTask(input, input.agent_bundle || {}) : undefined,
    parent_request: input.parent_request,
  }).filter(([, value]) => value !== '' && value !== undefined && !(Array.isArray(value) && value.length === 0)));
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
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
            metadata: { source: 'homeboy_allowed_tools' },
          };
        })
      : [{
          id: 'homeboy/no-runtime-tools',
          runtime_tool_id: 'homeboy_no_runtime_tools',
          execution_location: 'external',
          transport_visibility: 'hidden',
          allowed: false,
          metadata: { source: 'homeboy_default_empty_policy' },
        }],
    metadata: { source: 'homeboy-wp-codebox-task-runner' },
  };
}

function isAgentBundle(input) {
  return Boolean(input.agent_bundle && Object.keys(input.agent_bundle).length > 0);
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
  const outputs = {
    ...(plainObject(bundleRun.outputs) ? bundleRun.outputs : {}),
    ...toolRecorderOutputs(bundleRun.engine_data, config),
  };
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

  if (result.error && result.error.code === 'ETIMEDOUT') {
    process.stdout.write(`${JSON.stringify(timeoutPayload(timeoutMs, artifacts, evidencePath, inputPath, resolved.command, resolved.args), null, 2)}\n`);
    return 1;
  }

  if (result.stdout) {
    try {
      const payload = normalizeAgentTaskRun(input, JSON.parse(result.stdout));
      process.stdout.write(`${JSON.stringify(payload, null, 2)}\n`);
      return payload.success === false ? 1 : 0;
    } catch {
      process.stdout.write(result.stdout);
    }
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
