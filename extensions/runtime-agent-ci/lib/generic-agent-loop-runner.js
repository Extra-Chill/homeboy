'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { runDeterministicLoop } = require('./deterministic-loop-runner');
const { localOnlyReviewerFacingRef, validateControllerLoopProof } = require('./controller-loop-proof-validator');
const {
  CONTINUE,
  evaluateLoopPolicy,
  loopPolicyMaxRevolutions,
  normalizeLoopPolicy,
} = require('./loop-policy');
const { runtimeAgentCiTaskExecutorConfig } = require('./runtime-agent-ci-plan');
const { evaluateGatePlan } = require('./gate-plan-evaluator');
const { assertLoopSuccess, loopEvidence, loopIteration, loopRun } = require('./loop-lifecycle.cjs');
const { artifactManifestForFiles, runtimeAgentArtifactPaths } = require('./artifact-paths.cjs');

const DEFAULT_STDIO_SUMMARY_BYTES = 8192;
const DEFAULT_RUNTIME_ENV_ALLOWLIST = [
  'CI',
  'HOME',
  'HOMEBOY_RUNTIME_AGENT_DEBUG',
  'HOMEBOY_RUNTIME_AGENT_RUN_DIR',
  'LANG',
  'LC_ALL',
  'LOGNAME',
  'NODE_OPTIONS',
  'PATH',
  'PWD',
  'SHELL',
  'TMPDIR',
  'USER',
];

// Diagnostics mode. When enabled, the runtime executor subprocess stderr is
// fd-inherited so the full spawn chain (executor -> task-runner -> wp-codebox
// CLI) streams raw stderr straight to this process's stderr — which, since this
// runner shares the process that the CI step launches, lands directly in the
// job log in real time, even on a hard crash or timeout that produces no
// structured outcome.
function runtimeAgentDebugEnabled(env = process.env) {
  return ['1', 'true', 'yes', 'on'].includes(String(env.HOMEBOY_RUNTIME_AGENT_DEBUG || '').trim().toLowerCase());
}

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
  const loop = runGenericDeterministicLoop({
    loopId: request.task_id,
    maxIterations: options.maxIterations || options.max_iterations || options.loop?.maxIterations || options.loop?.max_iterations || 1,
    state: { request },
    buildTask: ({ state }) => state.request,
    executeTask: ({ task }) => normalizeOutcome(execute({ ...options, request: task, runtime }), task),
    collectResult: ({ outcome }) => outcome,
    reconcile: ({ state, result, artifacts, results, evidence }) => ({
      ...state,
      status: result?.status || 'failed',
      outcome: result,
      artifacts,
      results,
      evidence,
    }),
    shouldContinue: options.shouldContinue || options.should_continue || (() => false),
    stopPolicy: options.stopPolicy || options.stop_policy,
  });
  const outcome = loop.outcome || normalizeOutcome(null, request);
  let contractValidation = null;
  let contractOutcome = outcome;
  try {
    contractValidation = validateGenericAgentLoopOutcomeContract({
      request,
      outcome,
      runtime,
      plan: options.plan || {},
      validationPolicy,
    });
  } catch (error) {
    contractOutcome = outcomeWithContractValidationFailure(outcome, request, error);
  }
  const results = materializeGenericAgentLoopResults(contractOutcome, { ...options, runtime });
  const controllerProofRequested = options.controllerProof === true || options.controller_proof === true;
  const productionProof = controllerProofRequested ? buildBoundedProductionProof({ request, outcome: contractOutcome, plan: options.plan || {}, validationPolicy }) : null;
  const controllerProofValidation = controllerProofRequested ? validateControllerLoopProof({
    spec: buildControllerLoopProofSpec({ request, plan: options.plan || {}, validationPolicy }),
    proof: productionProof,
    policy: controllerProofPolicy(validationPolicy, options.plan || {}),
  }) : null;
  if (controllerProofRequested) {
    attachFullRunProofValidation(results, { productionProof, controllerProofValidation });
  }
  const assertion = options.validate === false ? null : assertGenericAgentLoopOutcome(results, validationPolicy);
  if (options.validate !== false && controllerProofRequested && !controllerProofValidation.valid) {
    throw new Error(`controller loop proof validation failed: ${controllerProofValidation.failures.map((item) => item.message).join('; ')}`);
  }
  return { request, outcome: contractOutcome, results, assertion, loop, productionProof, controllerProofValidation, contractValidation };
}

function runGenericDeterministicLoop(options = {}) {
  const loopId = options.loopId || options.loop_id || 'generic-deterministic-loop';
  const buildTask = options.buildTask || options.build_task || options.buildIteration || options.build_iteration || defaultBuildTask;
  const executeTask = requiredFunction(options.executeTask || options.execute_task || options.execute, 'executeTask');
  const collectResult = options.collectResult || options.collect_result || defaultCollectResult;
  const reconcile = options.reconcile || defaultGenericReconcile;
  const shouldContinue = options.shouldContinue || options.should_continue || defaultShouldContinue;
  const stopPolicy = options.stopPolicy || options.stop_policy || defaultStopPolicy;
  const loopPolicy = normalizeLoopPolicy(options, { defaultMode: 'count', defaultMaxRevolutions: 1 });
  const maxIterations = loopPolicyMaxRevolutions(loopPolicy, {
    nonCountMaxRevolutions: options.maxSynchronousRevolutions || options.max_synchronous_revolutions,
    requireNonCountMaxRevolutions: true,
  });
  const initialState = optionalObject(options.state || options.initialState || options.initial_state);
  const tasks = [];
  const results = [];
  const evidence = [];
  const loop = runDeterministicLoop({
    loopId,
    loop_policy: loopPolicy,
    maxIterations,
    maxSynchronousRevolutions: maxIterations,
    maxAttempts: options.maxAttempts || options.max_attempts || options.retry?.max_attempts,
    durationMs: loopPolicy.duration_ms,
    deadlineAt: loopPolicy.deadline_at,
    now: options.now,
    state: {
      ...initialState,
      tasks,
      results,
      evidence,
    },
    buildIteration: ({ loop_id, iteration, state, iterations }) => {
      const task = buildTask({ loop_id, loopId: loop_id, iteration, state, iterations, tasks, results, evidence });
      tasks.push(task);
      return task;
    },
    execute: ({ loop_id, iteration, attempt, input, state, iterations }) => executeTask({
      loop_id,
      loopId: loop_id,
      iteration,
      attempt,
      task: input,
      input,
      state,
      iterations,
      tasks,
      results,
      evidence,
    }),
    reconcile: ({ loop_id, iteration, attempt, input, outcome, error, artifacts, state, iterations }) => {
      const result = collectResult({
        loop_id,
        loopId: loop_id,
        iteration,
        attempt,
        task: input,
        outcome,
        error,
        artifacts,
        state,
        iterations,
        tasks,
        results,
        evidence,
      });
      results.push(result);
      evidence.push(...collectEvidence({ iteration, outcome, result, artifacts }));
      const nextState = reconcile({
        loop_id,
        loopId: loop_id,
        iteration,
        attempt,
        task: input,
        result,
        outcome,
        error,
        artifacts,
        state,
        iterations,
        tasks,
        results,
        evidence,
      });
      return isPlainObject(nextState) ? {
        ...nextState,
        tasks,
        results,
        evidence,
      } : {
        ...state,
        tasks,
        results,
        evidence,
      };
    },
    stopCriteria: (context) => evaluateGenericLoopGateDecision({
      context,
      stopPolicy,
      shouldContinue,
      tasks,
      results,
      evidence,
      maxIterations,
      loopPolicy,
      now: options.now,
    }),
  });
  const finalOutcome = results[results.length - 1] || null;
  return {
    ...loop,
    schema: 'homeboy/generic-deterministic-loop-output/v1',
    deterministic_loop_schema: loop.schema,
    outcome: finalOutcome,
    tasks,
    results,
    evidence,
    loop_policy: loopPolicy,
    evidence_envelope: {
      schema: 'homeboy/generic-deterministic-loop-evidence/v1',
      loop_id: loopId,
      status: loop.status,
      iteration_count: loop.iterations.length,
      task_count: tasks.length,
      result_count: results.length,
      evidence,
      loop_run: loopRun({
        loop_id: loopId,
        status: loop.status,
        stop_reason: loop.iterations[loop.iterations.length - 1]?.stop?.reason || '',
        max_iterations: maxIterations,
        iterations: loop.iterations.map((entry) => loopIteration({
          loop_id: loopId,
          iteration: entry.iteration,
          task: entry.input,
          result: entry.outcome,
          artifacts: entry.artifacts,
          gate_result: entry.stop?.data?.gate_result,
          accepted: genericLoopIterationAccepted(entry),
        })),
        evidence,
      }),
    },
  };
}

function evaluateGenericLoopGateDecision(options = {}) {
  const gateContext = {
    ...options.context,
    task: options.context.input,
    result: options.results[options.results.length - 1],
    tasks: options.tasks,
    results: options.results,
    evidence: options.evidence,
    max_iterations: options.maxIterations,
    maxIterations: options.maxIterations,
  };
  const stop = options.stopPolicy(gateContext);
  const stopGate = evaluateGatePlan({
    id: 'generic_loop_stop_policy',
    stop_when: [{ field: 'stop_requested', op: 'truthy', reason: isPlainObject(stop) ? stop.reason || 'stop_criteria_satisfied' : 'stop_criteria_satisfied' }],
  }, { stop_requested: isPlainObject(stop) ? stop.stop : Boolean(stop) });
  if (stopGate.action === 'stop') {
    return { stop: true, reason: stopGate.reason, data: { gate_result: stopGate } };
  }

  const policyStatus = evaluateLoopPolicy(options.loopPolicy, {
    completed_revolutions: gateContext.iteration,
    started_at: options.context.started_at || options.context.startedAt,
    now: options.now,
    cancelled: options.context.cancelled,
    cancellation_signal: options.context.cancellation_signal || options.context.cancellationSignal,
  });
  if (policyStatus.reason !== CONTINUE) {
    return { stop: true, reason: policyStatus.reason, data: { loop_policy_status: policyStatus } };
  }

  const continueDecision = options.shouldContinue(gateContext);
  const continueGate = evaluateGatePlan({
    id: 'generic_loop_continue_policy',
    continue_when: [{ field: 'continue_requested', op: 'truthy', reason: isPlainObject(continueDecision) ? continueDecision.reason || 'continuation_declined' : 'continuation_declined' }],
  }, { continue_requested: !(continueDecision === false || (isPlainObject(continueDecision) && continueDecision.continue === false)) });
  if (continueGate.action === 'stop') {
    return { stop: true, reason: continueGate.reason, data: { gate_result: continueGate } };
  }
  return { stop: false, data: { gate_result: continueGate } };
}

function executeRuntimeProvider(options = {}) {
  const runtime = requiredObject(options.runtime, 'runtime');
  const invocation = runtimeExecutorInvocation(runtime);
  if (!invocation.command) {
    throw new Error(`Runtime ${runtime.id || '(unknown)'} does not declare an executor command.`);
  }
  const spawn = options.spawnSync || spawnSync;
  const debug = runtimeAgentDebugEnabled(options.env) || runtimeAgentDebugEnabled(process.env);
  const result = spawn(invocation.command, invocation.argv || [], {
    encoding: 'utf8',
    cwd: invocation.cwd || process.cwd(),
    input: invocationStdin(invocation, options.request),
    env: runtimeInvocationEnv({ ...options, invocation }),
    maxBuffer: 1024 * 1024 * 20,
    // In debug mode inherit the executor stderr so the entire spawn chain's
    // raw stderr streams live to the job log, independent of any structured
    // outcome (which is never produced on a hard crash). stdout stays piped to
    // capture the JSON outcome.
    ...(debug ? { stdio: ['pipe', 'pipe', 'inherit'] } : {}),
  });
  const stderrArtifact = handleRuntimeInvocationStderr(result.stderr, { ...options, invocation, result });
  const stdout = String(result.stdout || '');
  if (!stdout.trim()) {
    return {
      schema: 'homeboy/agent-task-outcome/v1',
      task_id: options.request.task_id,
      status: 'failed',
      summary: 'Runtime agent task executor produced no JSON outcome.',
      diagnostics: [{
        class: 'homeboy.agent_loop.no_outcome',
        message: 'Runtime agent task executor produced no JSON outcome.',
        data: compactObject({ exit_status: result.status ?? 1, stderr_artifact: stderrArtifact }),
      }],
      metadata: compactObject({ runtime_invocation_stderr_artifact: stderrArtifact }),
    };
  }
  const outcome = JSON.parse(stdout);
  const failureArtifact = surfaceFailedRuntimeOutcome(outcome, { ...options, invocation, result, stderrArtifact });
  return captureInvocationResult(outcome, invocation, result, { stderrArtifact: failureArtifact || stderrArtifact });
}

function isFailedRuntimeOutcome(outcome) {
  if (!outcome || typeof outcome !== 'object' || Array.isArray(outcome)) {
    return false;
  }
  if (outcome.success === false) {
    return true;
  }
  return ['failed', 'error'].includes(String(outcome.status || '').toLowerCase());
}

function failedRuntimeOutcomeDetail(outcome) {
  const lines = [];
  if (outcome.summary) {
    lines.push(`summary: ${outcome.summary}`);
  }
  for (const diagnostic of normalizeArray(outcome.diagnostics)) {
    const diagnosticClass = diagnostic.class || diagnostic.kind || diagnostic.code || '';
    lines.push(`diagnostic${diagnosticClass ? ` [${diagnosticClass}]` : ''}: ${diagnostic.message || ''}`);
    const detail = diagnostic.data && typeof diagnostic.data === 'object'
      ? (diagnostic.data.stderr || diagnostic.data.error || '')
      : '';
    if (detail) {
      lines.push(String(detail));
    }
  }
  return lines.join('\n').trim();
}

// Surface the real failure reason from a failed runtime outcome to the job's
// stderr (so it lands in CI logs) and persist it as the runtime stderr artifact.
// The runtime executor routes the underlying agent/CLI stderr into the outcome's
// diagnostics rather than its own stderr, so without this the only thing a hard
// failure leaves behind is a later generic assertion message — the actual crash
// reason is lost because results.json is never written once the loop throws.
function surfaceFailedRuntimeOutcome(outcome, options = {}) {
  if (!isFailedRuntimeOutcome(outcome)) {
    return options.stderrArtifact || null;
  }
  const detail = failedRuntimeOutcomeDetail(outcome);
  if (!detail) {
    return options.stderrArtifact || null;
  }
  if (options.stderr !== false) {
    process.stderr.write(`${boundedText(detail, options.stderrMaxBytes || options.stderr_max_bytes || DEFAULT_STDIO_SUMMARY_BYTES)}\n`);
  }
  if (options.stderrArtifact) {
    return options.stderrArtifact;
  }
  const artifact = persistRuntimeInvocationStderr(detail, options);
  if (artifact && options.stderr !== false) {
    process.stderr.write(`Runtime failure detail artifact: ${artifact.path}\n`);
  }
  return artifact;
}

function runtimeInvocationEnv(options = {}) {
  const ambient = optionalObject(options.env || process.env);
  const request = optionalObject(options.request);
  const executor = optionalObject(request.executor);
  const config = optionalObject(executor.config);
  const invocation = optionalObject(options.invocation);
  assertNoAmbientEnvInheritance(invocation, config);

  const names = new Set([
    ...DEFAULT_RUNTIME_ENV_ALLOWLIST,
    ...normalizeArray(config.env_allowlist),
    ...normalizeArray(config.envAllowlist),
    ...normalizeArray(invocation.env_allowlist),
    ...normalizeArray(invocation.envAllowlist),
    ...normalizeArray(config.runtime_env_allowlist || config.runtimeEnvAllowlist),
    ...normalizeArray(config.secret_env),
    ...normalizeArray(executor.secret_env),
  ]);
  const env = {};
  for (const name of names) {
    if (typeof name === 'string' && name && ambient[name] !== undefined) {
      env[name] = ambient[name];
    }
  }
  return { ...env, ...optionalObject(config.runtime_env), ...optionalObject(invocation.env) };
}

function handleRuntimeInvocationStderr(stderr, options = {}) {
  const content = String(stderr || '');
  const invocation = options.invocation || {};
  const shouldPrint = content && options.stderr !== false && ((options.result?.status ?? 0) !== 0 || invocation.stderr === 'inherit');
  if (!content) {
    return null;
  }
  const artifact = persistRuntimeInvocationStderr(content, options);
  if (shouldPrint) {
    process.stderr.write(`${boundedText(content, options.stderrMaxBytes || options.stderr_max_bytes || DEFAULT_STDIO_SUMMARY_BYTES)}\n`);
    if (artifact) {
      process.stderr.write(`Runtime stderr artifact: ${artifact.path}\n`);
    }
  }
  return artifact;
}

function persistRuntimeInvocationStderr(content, options = {}) {
  const artifactDir = runtimeAgentArtifactPaths(options).run_dir;
  if (!artifactDir) {
    return null;
  }
  const taskId = options.request?.task_id || options.plan?.workload_id || options.plan?.task_id || 'runtime-agent-task';
  const filePath = runtimeAgentArtifactPaths({ ...options, stderrFile: options.stderrFile || options.stderr_file }).stderr || path.join(artifactDir, `${safeFileSegment(taskId)}-runtime-stderr.txt`);
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, content);
  return {
    kind: 'runtime-stderr',
    path: filePath,
    bytes: Buffer.byteLength(content),
  };
}

function boundedText(value, maxBytes = DEFAULT_STDIO_SUMMARY_BYTES) {
  const content = String(value || '');
  const limit = positiveInteger(maxBytes) || DEFAULT_STDIO_SUMMARY_BYTES;
  if (Buffer.byteLength(content) <= limit) {
    return content.replace(/\s+$/u, '');
  }
  const buffer = Buffer.from(content);
  const truncated = buffer.subarray(0, limit).toString('utf8').replace(/\s+$/u, '');
  return `${truncated}\n[truncated ${Buffer.byteLength(content) - Buffer.byteLength(truncated)} bytes; see artifact for full output]`;
}

function safeFileSegment(value) {
  return String(value || 'runtime-agent-task').replace(/[^a-z0-9._-]+/gi, '-').replace(/^-+|-+$/g, '') || 'runtime-agent-task';
}

function runtimeExecutorInvocation(runtime = {}) {
  const executor = requiredObject(runtime.executor, 'runtime.executor');
  if (executor.invocation && typeof executor.invocation === 'object' && !Array.isArray(executor.invocation)) {
    return {
      command: executor.invocation.command || '',
      argv: normalizeArray(executor.invocation.argv),
      cwd: executor.invocation.cwd || process.cwd(),
      env: optionalObject(executor.invocation.env),
      stdin: executor.invocation.stdin || 'request_json',
      stdout: executor.invocation.stdout || 'outcome_json',
      stderr: executor.invocation.stderr || 'inherit_on_failure',
      inherit_env: executor.invocation.inherit_env === true || executor.invocation.inheritEnv === true,
      env_allowlist: normalizeArray(executor.invocation.env_allowlist || executor.invocation.envAllowlist),
      artifacts: executor.invocation.artifacts || {},
      results: executor.invocation.results || {},
    };
  }
  if (executor.path) {
    return {
      command: process.execPath,
      argv: [executor.path],
      cwd: process.cwd(),
      env: {},
      stdin: 'request_json',
      stdout: 'outcome_json',
      stderr: 'inherit_on_failure',
      artifacts: {},
      results: {},
    };
  }
  return {};
}

function assertNoAmbientEnvInheritance(invocation = {}, config = {}) {
  if (invocation.inherit_env === true || invocation.inheritEnv === true || config.inherit_env === true || config.inheritEnv === true) {
    throw new Error('Runtime executor ambient env inheritance is not supported; declare env_allowlist, runtime_env, and secret_env explicitly.');
  }
}

function invocationStdin(invocation, request) {
  if (invocation.stdin === false || invocation.stdin === 'none') {
    return undefined;
  }
  return JSON.stringify(request);
}

function captureInvocationResult(outcome, invocation, result, options = {}) {
  const metadata = optionalObject(outcome.metadata);
  const invocationResult = {
    command: invocation.command,
    argv: invocation.argv || [],
    cwd: invocation.cwd || '',
    exit_status: result.status ?? 0,
    signal: result.signal || null,
    stderr_artifact: options.stderrArtifact || undefined,
  };
  return {
    ...outcome,
    metadata: {
      ...metadata,
      runtime_invocation_result: invocationResult,
    },
  };
}

function defaultBuildTask({ state }) {
  return state.task || state.input || state.request || state;
}

function defaultCollectResult({ outcome }) {
  return outcome;
}

function defaultGenericReconcile({ state, result, artifacts, results, evidence }) {
  return {
    ...state,
    status: result?.status || state.status || '',
    outcome: result,
    artifacts,
    results,
    evidence,
  };
}

function defaultShouldContinue() {
  return false;
}

function defaultStopPolicy({ iteration, maxIterations }) {
  return iteration >= maxIterations
    ? { stop: true, reason: 'max_revolutions_reached' }
    : { stop: false };
}

function collectEvidence({ iteration, outcome, result, artifacts }) {
  const resultEvidence = result === outcome ? [] : normalizeArray(result?.evidence_refs || result?.evidence);
  return [
    ...normalizeArray(artifacts).map((artifact) => loopEvidence({
      kind: 'artifact',
      iteration,
      uri: artifact.path || artifact.url || artifact.id || artifact.name || '',
      artifact,
    })),
    ...normalizeArray(outcome?.evidence_refs || outcome?.evidence).map((ref) => loopEvidence({
      kind: ref.kind || 'evidence_ref',
      iteration,
      uri: evidenceRefUrl(ref),
      evidence: ref,
    })),
    ...resultEvidence.map((ref) => loopEvidence({
      kind: ref.kind || 'evidence_ref',
      iteration,
      uri: evidenceRefUrl(ref),
      evidence: ref,
    })),
  ];
}

function genericLoopIterationAccepted(entry = {}) {
  const outcome = optionalObject(entry.outcome);
  const state = optionalObject(entry.state);
  if (outcome.accepted === false || state.accepted === false || outcome.success === false || outcome.status === 'failed') {
    return false;
  }
  if (outcome.accepted === true || state.accepted === true || outcome.success === true) {
    return true;
  }
  const status = outcome.status || outcome.state || '';
  return ['accepted', 'succeeded', 'passed', 'no_op'].includes(status);
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
  return assertLoopSuccess({
    results,
    scenario_id: validationPolicy.scenario_id || validationPolicy.workload_id || validationPolicy.flow_slug || '',
    success_requires_pr: validationPolicy.success_requires_pr,
    success_completion_outcomes: validationPolicy.success_completion_outcomes,
  });
}

function buildBoundedProductionProof(options = {}) {
  const { runBoundedProductionLoop } = require('./bounded-production-loop-runner');
  const request = requiredObject(options.request, 'request');
  const outcome = requiredObject(options.outcome, 'outcome');
  const plan = optionalObject(options.plan);
  const validationPolicy = optionalObject(options.validationPolicy || options.validation_policy);
  const proofPolicy = controllerProofPolicy(validationPolicy, plan);
  return runBoundedProductionLoop({
    loopId: `${request.task_id}-bounded-production-proof`,
    maxIterations: positiveInteger(proofPolicy.max_iterations) || 1,
    executeIteration: () => outcome,
    acceptedStatuses: normalizeArray(proofPolicy.accepted_statuses).length > 0
      ? proofPolicy.accepted_statuses
      : ['accepted', 'succeeded', 'passed', 'no_op'],
    artifactRequirements: artifactRequirementsForProof(request, plan, proofPolicy),
    evidenceRequirements: evidenceRequirementsForProof(validationPolicy, plan, proofPolicy),
    previewRequirement: proofPolicy.preview_required === true || proofPolicy.require_preview === true,
    publicationEvidenceRequirement: proofPolicy.publication_required === true || proofPolicy.require_publication === true || proofPolicy.pr_required === true || proofPolicy.require_pr === true,
  });
}

function buildControllerLoopProofSpec(options = {}) {
  const request = requiredObject(options.request, 'request');
  const plan = optionalObject(options.plan);
  const validationPolicy = optionalObject(options.validationPolicy || options.validation_policy);
  const proofPolicy = controllerProofPolicy(validationPolicy, plan);
  return {
    schema: 'homeboy/full-run-controller-loop-proof-spec/v1',
    artifacts: artifactDeclarationsForControllerProof(request, plan, proofPolicy),
    required_evidence: evidenceRequirementsForProof(validationPolicy, plan, proofPolicy),
    policy: {
      ...proofPolicy,
      allowed_statuses: normalizeArray(proofPolicy.allowed_statuses).length > 0 ? proofPolicy.allowed_statuses : ['succeeded'],
      allowed_stop_reasons: normalizeArray(proofPolicy.allowed_stop_reasons).length > 0 ? proofPolicy.allowed_stop_reasons : ['accepted'],
      max_iterations: positiveInteger(proofPolicy.max_iterations) || 1,
      publication_evidence: proofPolicy.publication_evidence || proofPolicy.pr_evidence || { kind: 'publication' },
    },
  };
}

function controllerProofPolicy(validationPolicy = {}, plan = {}) {
  return {
    ...optionalObject(plan.controller_loop_proof),
    ...optionalObject(plan.controller_loop_proof_policy),
    ...optionalObject(validationPolicy.controller_loop_proof),
    ...optionalObject(validationPolicy.controller_loop_proof_policy),
  };
}

function artifactRequirementsForProof(request, plan, proofPolicy) {
  const expected = normalizeArray(request.expected_artifacts).map((name) => ({ name }));
  const declared = artifactDeclarationsForControllerProof(request, plan, proofPolicy)
    .filter((declaration) => declaration.required)
    .map((declaration) => ({ name: declaration.id, kind: declaration.kind, role: declaration.role }));
  return [...expected, ...declared];
}

function artifactDeclarationsForControllerProof(request, plan, proofPolicy) {
  const declarations = normalizeArtifactDeclarations([
    ...normalizeArray(request.artifact_declarations),
    ...normalizeArray(plan.artifact_declarations),
    ...normalizeArray(proofPolicy.artifacts || proofPolicy.artifact_declarations),
  ]);
  return declarations.map((declaration) => ({
    id: declaration.name,
    required: declaration.required,
    kind: declaration.kind,
    reviewer_facing: declaration.reviewer_facing,
    durable_url_required: declaration.durable_url_required,
  }));
}

function evidenceRequirementsForProof(validationPolicy, plan, proofPolicy) {
  return normalizeEvidenceRequirements([
    ...normalizeArray(plan.required_evidence_refs || plan.required_evidence),
    ...normalizeArray(validationPolicy.required_evidence_refs || validationPolicy.required_evidence),
    ...normalizeArray(proofPolicy.required_evidence_refs || proofPolicy.required_evidence),
  ]);
}

function attachFullRunProofValidation(results, proof) {
  const scenario = normalizeArray(results?.scenarios)[0];
  if (!scenario || typeof scenario !== 'object') {
    return;
  }
  scenario.metadata = {
    ...optionalObject(scenario.metadata),
    bounded_production_loop_proof: compactProductionProof(proof.productionProof),
    controller_loop_proof_validation: proof.controllerProofValidation,
  };
}

function compactProductionProof(proof) {
  return {
    schema: proof.schema,
    loop_id: proof.loop_id,
    status: proof.status,
    stop_reason: proof.stop_reason,
    max_iterations: proof.max_iterations,
    iteration_count: proof.iteration_count,
    validation_failures: proof.validation_failures,
    evidence_envelope: proof.evidence_envelope,
    iterations: normalizeArray(proof.iterations).map((iteration) => ({
      schema: iteration.schema,
      loop_id: iteration.loop_id,
      iteration: iteration.iteration,
      result: iteration.result,
      artifacts: iteration.artifacts,
      evidence_refs: iteration.evidence_refs,
      validation_failures: iteration.validation_failures,
      accepted: iteration.accepted,
      repair: iteration.repair,
      fanout: iteration.fanout,
    })),
  };
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
    const refValue = evidenceRefUrl(ref);
    if (localOnlyReviewerFacingRef(refValue)) {
      errors.push(`required evidence ref ${requirement.name || requirement.kind || requirement.url || requirement.uri || '(unnamed)'} uses local-only evidence: ${refValue}`);
    }
  }

  if (errors.length > 0) {
    throw new Error(`generic agent loop contract validation failed: ${errors.join('; ')}`);
  }
  return { artifact_count: artifacts.length, evidence_ref_count: evidenceRefs.length };
}

function writeGenericAgentLoopArtifacts(options = {}) {
  const artifactPaths = runtimeAgentArtifactPaths(options);
  const manifestFiles = [];
  if (artifactPaths.outcome) {
    writeJsonFile(artifactPaths.outcome, options.outcome);
    manifestFiles.push({ id: 'outcome', kind: 'agent-task-outcome', role: 'outcome', label: 'Agent task outcome', path: artifactPaths.outcome, content_type: 'application/json' });
  }
  if (artifactPaths.results) {
    writeJsonFile(artifactPaths.results, options.results);
    manifestFiles.push({ id: 'results', kind: 'runtime-agent-results', role: 'results', label: 'Runtime agent results', path: artifactPaths.results, content_type: 'application/json' });
  }
  if (artifactPaths.status) {
    writeJsonFile(artifactPaths.status, {
      schema: 'homeboy/runtime-agent-status/v1',
      task_id: options.outcome?.task_id || options.request?.task_id || '',
      status: options.outcome?.status || 'failed',
      success: ['succeeded', 'no_op'].includes(options.outcome?.status),
    });
    manifestFiles.push({ id: 'status', kind: 'runtime-agent-status', role: 'status', label: 'Runtime agent status', path: artifactPaths.status, content_type: 'application/json' });
  }
  for (const artifact of normalizeArray(options.outcome?.artifacts)) {
    if (artifact?.path) {
      manifestFiles.push(artifact);
    }
  }
  const stderrArtifact = options.outcome?.metadata?.runtime_invocation_result?.stderr_artifact || options.outcome?.metadata?.runtime_invocation_stderr_artifact;
  if (stderrArtifact?.path) {
    manifestFiles.push({ id: 'runtime_stderr', role: 'stderr', label: 'Runtime stderr', ...stderrArtifact });
  }
  if (artifactPaths.artifact_manifest) {
    writeJsonFile(artifactPaths.artifact_manifest, artifactManifestForFiles(artifactPaths, manifestFiles));
  }
}

function genericAgentLoopStdoutSummary(options = {}) {
  const outcome = optionalObject(options.outcome);
  const results = optionalObject(options.results);
  const outcomeFile = options.outcomeFile || options.outcome_file || '';
  const resultsFile = options.resultsFile || options.results_file || '';
  const artifacts = normalizeArray(outcome.artifacts);
  const evidenceRefs = normalizeArray(outcome.evidence_refs || outcome.evidence);
  const diagnostics = normalizeArray(outcome.diagnostics);
  return {
    schema: 'homeboy/agent-task-stdout-summary/v1',
    task_id: outcome.task_id || options.request?.task_id || '',
    status: outcome.status || '',
    summary: boundedText(outcome.summary || '', options.summaryMaxBytes || 2048),
    artifact_count: artifacts.length,
    evidence_ref_count: evidenceRefs.length,
    diagnostic_count: diagnostics.length,
    artifact_refs: artifacts.slice(0, 10).map(compactArtifactRef),
    evidence_refs: evidenceRefs.slice(0, 10).map(compactEvidenceRef),
    diagnostics: diagnostics.slice(0, 10).map(compactDiagnostic),
    result_scenario_count: normalizeArray(results.scenarios).length,
    files: compactObject({ outcome: outcomeFile, results: resultsFile }),
  };
}

function compactArtifactRef(artifact = {}) {
  return compactObject({
    id: artifact.id,
    name: artifact.name,
    kind: artifact.kind || artifact.type || artifact.artifact_kind || artifact.artifactKind,
    path: artifact.path,
    url: artifact.url,
    uri: artifact.uri,
  });
}

function compactEvidenceRef(ref = {}) {
  return compactObject({
    id: ref.id,
    kind: ref.kind || ref.type,
    label: ref.label || ref.name,
    url: ref.url,
    uri: ref.uri,
    path: ref.path,
  });
}

function compactDiagnostic(diagnostic = {}) {
  return compactObject({
    class: diagnostic.class || diagnostic.kind || diagnostic.code,
    message: boundedText(diagnostic.message || '', 2048),
  });
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
      reviewer_facing: declaration.reviewer_facing,
      durable_url_required: declaration.durable_url_required,
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

function evidenceRefUrl(ref) {
  return ref.url || ref.uri || ref.href || ref.path || '';
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
    ability_input: optionalObject(runtimeTask.input),
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

function outcomeWithContractValidationFailure(outcome, request, error) {
  const normalized = normalizeOutcome(outcome, request);
  return {
    ...normalized,
    schema: normalized.schema || 'homeboy/agent-task-outcome/v1',
    task_id: normalized.task_id || request.task_id,
    status: 'failed',
    summary: errorMessage(error),
    diagnostics: [
      ...(Array.isArray(normalized.diagnostics) ? normalized.diagnostics : []),
      {
        class: 'homeboy.generic_agent_loop.contract_validation_failed',
        message: errorMessage(error),
        data: {
          original_status: normalized.status || '',
          original_summary: normalized.summary || '',
        },
      },
    ],
    metadata: {
      ...(optionalObject(normalized.metadata)),
      generic_agent_loop_contract_validation: {
        schema: 'homeboy/generic-agent-loop-contract-validation/v1',
        valid: false,
        error: errorMessage(error),
        invalid_candidate_outcome: normalized,
      },
    },
  };
}

function errorMessage(error) {
  return error && typeof error.message === 'string' && error.message.trim()
    ? error.message
    : String(error || 'generic agent loop contract validation failed');
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

function requiredFunction(value, name) {
  if (typeof value !== 'function') {
    throw new Error(`${name} must be a function.`);
  }
  return value;
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function nonEmpty(value) {
  return value !== undefined && value !== null && value !== '';
}

function isPlainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function firstString(...values) {
  return values.find((value) => typeof value === 'string' && value.trim() !== '') || '';
}

module.exports = {
  assertGenericAgentLoopOutcome,
  assertGenericAgentLoopRuntimeContract,
  buildGenericAgentLoopRequest,
  buildBoundedProductionProof,
  buildControllerLoopProofSpec,
  controllerProofPolicy,
  materializeGenericAgentLoopResults,
  genericAgentLoopStdoutSummary,
  runGenericAgentLoop,
  runGenericDeterministicLoop,
  validateGenericAgentLoopOutcomeContract,
  writeGenericAgentLoopArtifacts,
};
