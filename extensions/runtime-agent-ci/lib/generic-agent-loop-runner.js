'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { runDeterministicLoop } = require('./deterministic-loop-runner');
const { runtimeAgentCiTaskExecutorConfig } = require('./runtime-agent-ci-plan');

function buildGenericAgentLoopRequest(options = {}) {
  const plan = requiredObject(options.plan, 'plan');
  const runtime = requiredObject(options.runtime, 'runtime');
  assertGenericAgentLoopRuntimeContract(plan, runtime);
  const configPath = options.configPath || options.config_path || '';
  const taskId = plan.task_id || plan.workload_id || 'generic-agent-loop';
  const runtimeComponents = optionalObject(plan.runtime_components);
  const runtimeComponentPaths = Object.fromEntries(Object.entries({
    ...optionalObject(plan.runtime_component_paths),
    runtime: runtimeComponents.runtime,
  }).filter(([, value]) => nonEmpty(value)));
  const executorConfig = runtimeAgentCiTaskExecutorConfig(Object.fromEntries(Object.entries({
    ...plan,
    ...runtimeTaskOptions(plan),
    runtime_component_paths: runtimeComponentPaths,
    homeboy_extensions: plan.homeboy_extensions || plan.homeboy_extensions_path || options.extensionPath || options.extension_path,
    artifacts: plan.artifacts_path || plan.artifacts,
    replay_bundle_dir: plan.replay_bundle_dir || options.replayBundleDir || options.replay_bundle_dir,
  }).filter(([, value]) => nonEmpty(value))));

  return {
    schema: 'homeboy/agent-task-request/v1',
    task_id: String(taskId),
    group_key: plan.group_key || plan.workload_id || '',
    instructions: plan.prompt || plan.workload_label || 'Run agent task.',
    source_refs: configPath ? [{ kind: 'config', path: configPath }] : [],
    workspace: compactObject({ repository: plan.target_repo, path: plan.component_path }),
    expected_artifacts: expectedArtifactsFromPlan(plan),
    artifact_declarations: normalizeArray(plan.artifact_declarations),
    policy: optionalObject(plan.policy),
    limits: compactObject({
      timeout_ms: positiveInteger(plan.time_budget_ms),
      task_timeout_seconds: positiveInteger(plan.task_timeout_seconds || plan.taskTimeoutSeconds),
    }),
    inputs: {
      target: compactObject({ repository: plan.target_repo, path: plan.component_path }),
      context: compactObject({ config_path: configPath, workflow_run_url: workflowRunUrl(options.env || process.env) }),
    },
    executor: {
      backend: runtime.executor.backend,
      model: plan.model || '',
      config: executorConfig,
      secret_env: plan.secret_env || [],
    },
  };
}

function runGenericAgentLoop(options = {}) {
  const runtime = requiredObject(options.runtime, 'runtime');
  const request = options.request || buildGenericAgentLoopRequest(options);
  const execute = options.execute || executeRuntimeProvider;
  const validationPolicy = options.validationPolicy || options.validation_policy || {};
  const loop = runDeterministicLoop({
    loopId: request.task_id,
    maxIterations: 1,
    state: { request },
    buildIteration: ({ state }) => state.request,
    execute: ({ input }) => normalizeOutcome(execute({ ...options, request: input, runtime }), input),
    reconcile: ({ state, outcome, artifacts }) => ({
      ...state,
      status: outcome?.status || 'failed',
      outcome,
      artifacts,
    }),
    stopCriteria: () => true,
  });
  const outcome = loop.iterations[0]?.outcome || normalizeOutcome(null, request);
  validateGenericAgentLoopOutcomeContract({
    request,
    outcome,
    runtime,
    plan: options.plan || {},
    validationPolicy,
  });
  const results = materializeGenericAgentLoopResults(outcome, { ...options, runtime });
  const assertion = options.validate === false ? null : assertGenericAgentLoopOutcome(results, validationPolicy);
  return { request, outcome, results, assertion, loop };
}

function executeRuntimeProvider(options = {}) {
  const runtime = requiredObject(options.runtime, 'runtime');
  if (!runtime.executor.path) {
    throw new Error(`Runtime ${runtime.id || '(unknown)'} does not declare an executor path.`);
  }
  const spawn = options.spawnSync || spawnSync;
  const result = spawn(process.execPath, [runtime.executor.path], {
    encoding: 'utf8',
    input: JSON.stringify(options.request),
    env: options.env || process.env,
    maxBuffer: 1024 * 1024 * 20,
  });
  if (result.stderr && options.stderr !== false) {
    process.stderr.write(result.stderr);
  }
  if (!result.stdout || !result.stdout.trim()) {
    return {
      schema: 'homeboy/agent-task-outcome/v1',
      task_id: options.request.task_id,
      status: 'failed',
      summary: 'Runtime agent task executor produced no JSON outcome.',
      diagnostics: [{
        class: 'homeboy.agent_loop.no_outcome',
        message: 'Runtime agent task executor produced no JSON outcome.',
        data: { exit_status: result.status ?? 1 },
      }],
    };
  }
  return JSON.parse(result.stdout);
}

function materializeGenericAgentLoopResults(outcome, options = {}) {
  const adapter = resolveOutcomeAdapter(options.runtime, options.repoRoot || options.repo_root);
  if (adapter) {
    const results = adapter(outcome, options);
    if (results && Array.isArray(results.scenarios)) {
      return results;
    }
  }
  const embedded = outcome?.metadata?.agent_loop_results || outcome?.metadata?.results || outcome?.results;
  if (embedded && Array.isArray(embedded.scenarios)) {
    return embedded;
  }
  return {
    scenarios: [{
      id: outcome.task_id || options.plan?.workload_id || 'generic-agent-loop',
      label: outcome.summary || 'Agent task',
      metrics: { generic_agent_task_executor_mean: 1 },
      metadata: {
        job_status: outcome.status,
        success_status: outcome.status,
        agent_task_outcome: outcome,
      },
    }],
  };
}

function assertGenericAgentLoopOutcome(results, validationPolicy = {}) {
  const scenarioId = validationPolicy.scenario_id || validationPolicy.workload_id || validationPolicy.flow_slug || '';
  const scenario = findScenario(results, scenarioId);
  const metadata = scenario.metadata || {};
  const jobStatus = metadata.job_status || '';
  const successStatus = metadata.success_status || '';
  const errorMessage = metadata.error_message || '';
  const noChangesAllowed = validationPolicy.success_requires_pr === false;
  const allowedCompletionOutcomes = normalizeArray(validationPolicy.success_completion_outcomes);
  const completionOutcome = metadata.completion_outcome || metadata.completionOutcome || '';
  const completionOutcomeSatisfied = metadata.completion_outcome_satisfied === true || Boolean(completionOutcome && allowedCompletionOutcomes.includes(completionOutcome));
  const assertion = {
    scenario_id: scenario.id || scenarioId,
    job_status: jobStatus,
    success_status: successStatus,
    error_message: errorMessage,
    completion_outcome_satisfied: completionOutcomeSatisfied,
    no_changes_allowed: noChangesAllowed,
  };

  if (errorMessage) {
    throw new Error(`scenario ${assertion.scenario_id} completed with error_message=${errorMessage}`);
  }
  if (successStatus === 'pr_opened' || completionOutcomeSatisfied || (successStatus === 'no_changes' && noChangesAllowed)) {
    return assertion;
  }
  throw new Error(`scenario ${assertion.scenario_id} expected opened PR, satisfied completion outcome, or allowed no-changes result, got job_status=${jobStatus} success_status=${successStatus} completion_outcome_satisfied=${completionOutcomeSatisfied ? 'true' : 'false'} no_changes_allowed=${noChangesAllowed ? 'true' : 'false'}`);
}

function assertGenericAgentLoopRuntimeContract(plan = {}, runtime = {}) {
  const runtimeProfileId = requiredStringValue(plan.runtime_profile || plan.profile, 'runtime_profile');
  const runtimeProfiles = optionalObject(plan.runtime_profiles || plan.runtimeProfiles);
  if (!runtimeProfiles[runtimeProfileId]) {
    throw new Error(`runtime profile ${runtimeProfileId} is not declared in runtime_profiles.`);
  }

  const requiredCapabilities = normalizeArray(plan.required_runtime_capabilities || plan.required_capabilities);
  if (requiredCapabilities.length === 0) {
    return { runtime_profile: runtimeProfileId, missing_capabilities: [] };
  }

  const capabilities = runtimeCapabilities(runtime, plan);
  const missing = requiredCapabilities.filter((capability) => !capabilities.has(capability));
  if (missing.length > 0) {
    throw new Error(`runtime ${runtime.id || '(unknown)'} is missing required capabilities: ${missing.join(', ')}`);
  }
  return { runtime_profile: runtimeProfileId, missing_capabilities: [] };
}

function validateGenericAgentLoopOutcomeContract(options = {}) {
  const request = requiredObject(options.request, 'request');
  const outcome = requiredObject(options.outcome, 'outcome');
  const plan = optionalObject(options.plan);
  const validationPolicy = optionalObject(options.validationPolicy || options.validation_policy);
  const declarations = normalizeArtifactDeclarations(request.artifact_declarations || plan.artifact_declarations);
  const artifacts = normalizeArray(outcome.artifacts);
  const evidenceRefs = normalizeArray(outcome.evidence_refs || outcome.evidence);
  const errors = [];

  for (const expected of normalizeArray(request.expected_artifacts)) {
    if (!findArtifactByName(artifacts, expected)) {
      errors.push(`missing expected artifact ${expected}`);
    }
  }

  for (const declaration of declarations) {
    if (!declaration.required) {
      continue;
    }
    const artifact = findArtifactByName(artifacts, declaration.name);
    if (!artifact) {
      errors.push(`missing declared artifact ${declaration.name}`);
      continue;
    }
    if (declaration.kind && artifactKind(artifact) !== declaration.kind) {
      errors.push(`artifact ${declaration.name} expected kind ${declaration.kind}, got ${artifactKind(artifact) || '(missing)'}`);
    }
    if (declaration.schema && artifactSchema(artifact) !== declaration.schema) {
      errors.push(`artifact ${declaration.name} expected schema ${declaration.schema}, got ${artifactSchema(artifact) || '(missing)'}`);
    }
  }

  const requiredEvidenceRefs = normalizeEvidenceRequirements(validationPolicy.required_evidence_refs || plan.required_evidence_refs);
  for (const requirement of requiredEvidenceRefs) {
    const ref = findEvidenceRef(evidenceRefs, requirement);
    if (!ref) {
      errors.push(`missing required evidence ref ${requirement.name || requirement.kind || requirement.url || requirement.uri || '(unnamed)'}`);
      continue;
    }
    const localOnly = localOnlyReviewerEvidence(ref);
    if (localOnly) {
      errors.push(`evidence ref ${evidenceRefLabel(ref)} is local-only: ${localOnly}`);
    }
  }

  for (const ref of evidenceRefs) {
    const localOnly = localOnlyReviewerEvidence(ref);
    if (localOnly) {
      errors.push(`evidence ref ${evidenceRefLabel(ref)} is local-only: ${localOnly}`);
    }
  }

  if (errors.length > 0) {
    throw new Error(`generic agent loop contract validation failed: ${errors.join('; ')}`);
  }
  return { artifact_count: artifacts.length, evidence_ref_count: evidenceRefs.length };
}

function writeGenericAgentLoopArtifacts(options = {}) {
  if (options.outcomeFile) {
    writeJsonFile(options.outcomeFile, options.outcome);
  }
  if (options.resultsFile) {
    writeJsonFile(options.resultsFile, options.results);
  }
}

function expectedArtifactsFromPlan(plan) {
  const expected = normalizeArray(plan.expected_artifacts);
  if (expected.length > 0) {
    return expected;
  }
  return normalizeArray(plan.artifact_declarations)
    .filter((declaration) => declaration && typeof declaration === 'object' && declaration.required === true)
    .map((declaration) => declaration.name || declaration.id || '')
    .filter(Boolean);
}

function normalizeArtifactDeclarations(value) {
  return normalizeArray(value)
    .filter((declaration) => declaration && typeof declaration === 'object')
    .map((declaration) => compactObject({
      name: declaration.name || declaration.id,
      required: declaration.required === true,
      kind: declaration.kind || declaration.type || declaration.artifact_kind || declaration.artifactKind,
      schema: declaration.artifact_schema || declaration.artifactSchema || declaration.payload_schema || declaration.payloadSchema,
    }))
    .filter((declaration) => declaration.name);
}

function normalizeEvidenceRequirements(value) {
  return normalizeArray(value)
    .map((requirement) => typeof requirement === 'string' ? { name: requirement } : requirement)
    .filter((requirement) => requirement && typeof requirement === 'object' && !Array.isArray(requirement));
}

function findArtifactByName(artifacts, name) {
  return normalizeArray(artifacts).find((artifact) => artifact && typeof artifact === 'object' && (
    artifact.name === name || artifact.id === name || artifact.role === name
  ));
}

function findEvidenceRef(evidenceRefs, requirement) {
  return normalizeArray(evidenceRefs).find((ref) => ref && typeof ref === 'object' && (
    (requirement.name && (ref.name === requirement.name || ref.id === requirement.name || ref.label === requirement.name))
    || (requirement.kind && ref.kind === requirement.kind)
    || (requirement.url && evidenceRefUrl(ref) === requirement.url)
    || (requirement.uri && evidenceRefUrl(ref) === requirement.uri)
  ));
}

function artifactKind(artifact) {
  return artifact.kind || artifact.type || artifact.artifact_kind || artifact.artifactKind || '';
}

function artifactSchema(artifact) {
  return artifact.artifact_schema || artifact.artifactSchema || artifact.payload_schema || artifact.payloadSchema || artifact.schema || '';
}

function evidenceRefLabel(ref) {
  return ref.name || ref.id || ref.label || ref.kind || evidenceRefUrl(ref) || '(unnamed)';
}

function evidenceRefUrl(ref) {
  return ref.url || ref.uri || ref.href || ref.path || '';
}

function localOnlyReviewerEvidence(ref) {
  const url = evidenceRefUrl(ref);
  if (!url || typeof url !== 'string') {
    return '';
  }
  if (/^https?:\/\/(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(?::|\/|$)/i.test(url)) {
    return url;
  }
  if (/^file:\/\//i.test(url) || /^\/Users\//.test(url) || /^\/private\//.test(url) || /^\/tmp\//.test(url)) {
    return url;
  }
  return '';
}

function runtimeCapabilities(runtime = {}, plan = {}) {
  const capabilities = new Set(normalizeArray(runtime.capabilities));
  for (const capability of normalizeArray(runtime.manifest?.capabilities)) {
    capabilities.add(capability);
  }
  const executors = normalizeArray(runtime.manifest?.agent_task_executors);
  const backend = runtime.executor?.backend || plan.backend || plan.runtime_backend;
  const runtimeId = runtime.id || plan.runtime_id || plan.runtime;
  for (const executor of executors) {
    if ((!backend || executor.backend === backend) && (!runtimeId || !executor.runtime_id || executor.runtime_id === runtimeId)) {
      for (const capability of normalizeArray(executor.capabilities)) {
        capabilities.add(capability);
      }
    }
  }
  return capabilities;
}

function runtimeTaskOptions(plan) {
  const runtimeTask = optionalObject(plan.runtime_task);
  if (!runtimeTask.ability) {
    return {};
  }
  return {
    ability: runtimeTask.ability,
    abilityInput: optionalObject(runtimeTask.input),
  };
}

function resolveOutcomeAdapter(runtime, repoRoot) {
  const adapterConfig = runtime?.manifest?.agent_loop?.outcome_adapter;
  if (!adapterConfig || !adapterConfig.module) {
    return null;
  }
  const root = repoRoot || path.resolve(__dirname, '..', '..');
  const adapterModule = require(path.resolve(root, adapterConfig.module));
  const adapter = adapterConfig.export ? adapterModule[adapterConfig.export] : adapterModule;
  if (typeof adapter !== 'function') {
    throw new Error(`Runtime ${runtime.id} outcome adapter is not a function: ${adapterConfig.module}`);
  }
  return adapter;
}

function normalizeOutcome(value, request) {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value;
  }
  return {
    schema: 'homeboy/agent-task-outcome/v1',
    task_id: request.task_id,
    status: 'failed',
    summary: 'Runtime agent task executor produced an invalid outcome.',
  };
}

function findScenario(results, scenarioId) {
  const scenarios = normalizeArray(results?.scenarios);
  const scenario = scenarioId ? scenarios.find((candidate) => candidate.id === scenarioId) : scenarios[0];
  if (!scenario) {
    throw new Error(`scenario ${scenarioId || '(first)'} was not found in agent loop results.`);
  }
  return scenario;
}

function workflowRunUrl(env) {
  return env.GITHUB_SERVER_URL && env.GITHUB_REPOSITORY && env.GITHUB_RUN_ID
    ? `${env.GITHUB_SERVER_URL}/${env.GITHUB_REPOSITORY}/actions/runs/${env.GITHUB_RUN_ID}`
    : '';
}

function writeJsonFile(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function positiveInteger(value) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
}

function compactObject(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => nonEmpty(entry)));
}

function optionalObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function requiredObject(value, name) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${name} must be an object.`);
  }
  return value;
}

function requiredStringValue(value, name) {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`${name} is required.`);
  }
  return value;
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function nonEmpty(value) {
  return value !== undefined && value !== null && value !== '';
}

module.exports = {
  assertGenericAgentLoopOutcome,
  assertGenericAgentLoopRuntimeContract,
  buildGenericAgentLoopRequest,
  materializeGenericAgentLoopResults,
  runGenericAgentLoop,
  validateGenericAgentLoopOutcomeContract,
  writeGenericAgentLoopArtifacts,
};
