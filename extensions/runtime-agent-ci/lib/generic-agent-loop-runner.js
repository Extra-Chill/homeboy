'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { runDeterministicLoop } = require('./deterministic-loop-runner');
const { runBoundedProductionLoop } = require('./bounded-production-loop-runner');
const { validateControllerLoopProof } = require('./controller-loop-proof-validator');
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
  validateGenericAgentLoopOutcomeContract({
    request,
    outcome,
    runtime,
    plan: options.plan || {},
    validationPolicy,
  });
  const results = materializeGenericAgentLoopResults(outcome, { ...options, runtime });
  const productionProof = buildBoundedProductionProof({ request, outcome, plan: options.plan || {}, validationPolicy });
  const controllerProofValidation = validateControllerLoopProof({
    spec: buildControllerLoopProofSpec({ request, plan: options.plan || {}, validationPolicy }),
    proof: productionProof,
    policy: controllerProofPolicy(validationPolicy, options.plan || {}),
  });
  attachFullRunProofValidation(results, { productionProof, controllerProofValidation });
  const assertion = options.validate === false ? null : assertGenericAgentLoopOutcome(results, validationPolicy);
  if (options.validate !== false && !controllerProofValidation.valid) {
    throw new Error(`controller loop proof validation failed: ${controllerProofValidation.failures.map((item) => item.message).join('; ')}`);
  }
  return { request, outcome, results, assertion, loop, productionProof, controllerProofValidation };
}

function runGenericDeterministicLoop(options = {}) {
  const loopId = options.loopId || options.loop_id || 'generic-deterministic-loop';
  const buildTask = options.buildTask || options.build_task || options.buildIteration || options.build_iteration || defaultBuildTask;
  const executeTask = requiredFunction(options.executeTask || options.execute_task || options.execute, 'executeTask');
  const collectResult = options.collectResult || options.collect_result || defaultCollectResult;
  const reconcile = options.reconcile || defaultGenericReconcile;
  const shouldContinue = options.shouldContinue || options.should_continue || defaultShouldContinue;
  const stopPolicy = options.stopPolicy || options.stop_policy || defaultStopPolicy;
  const maxIterations = positiveInteger(options.maxIterations || options.max_iterations) || 1;
  const initialState = optionalObject(options.state || options.initialState || options.initial_state);
  const tasks = [];
  const results = [];
  const evidence = [];
  const loop = runDeterministicLoop({
    loopId,
    maxIterations,
    maxAttempts: options.maxAttempts || options.max_attempts || options.retry?.max_attempts,
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
    stopCriteria: (context) => {
      const stop = stopPolicy({
        ...context,
        task: context.input,
        result: results[results.length - 1],
        tasks,
        results,
        evidence,
        max_iterations: maxIterations,
        maxIterations,
      });
      if (isPlainObject(stop) ? stop.stop : Boolean(stop)) {
        return stop;
      }
      const continueDecision = shouldContinue({
        ...context,
        task: context.input,
        result: results[results.length - 1],
        tasks,
        results,
        evidence,
        max_iterations: maxIterations,
        maxIterations,
      });
      if (continueDecision === false || (isPlainObject(continueDecision) && continueDecision.continue === false)) {
        return { stop: true, reason: isPlainObject(continueDecision) ? continueDecision.reason || 'continuation_declined' : 'continuation_declined' };
      }
      return { stop: false };
    },
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
    evidence_envelope: {
      schema: 'homeboy/generic-deterministic-loop-evidence/v1',
      loop_id: loopId,
      status: loop.status,
      iteration_count: loop.iterations.length,
      task_count: tasks.length,
      result_count: results.length,
      evidence,
    },
  };
}

function executeRuntimeProvider(options = {}) {
  const runtime = requiredObject(options.runtime, 'runtime');
  const invocation = runtimeExecutorInvocation(runtime);
  if (!invocation.command) {
    throw new Error(`Runtime ${runtime.id || '(unknown)'} does not declare an executor command.`);
  }
  const spawn = options.spawnSync || spawnSync;
  const result = spawn(invocation.command, invocation.argv || [], {
    encoding: 'utf8',
    cwd: invocation.cwd || process.cwd(),
    input: invocationStdin(invocation, options.request),
    env: { ...(options.env || process.env), ...(invocation.env || {}) },
    maxBuffer: 1024 * 1024 * 20,
  });
  if (result.stderr && options.stderr !== false && (result.status !== 0 || invocation.stderr === 'inherit')) {
    process.stderr.write(result.stderr);
  }
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
        data: { exit_status: result.status ?? 1 },
      }],
    };
  }
  const outcome = JSON.parse(stdout);
  return captureInvocationResult(outcome, invocation, result);
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

function invocationStdin(invocation, request) {
  if (invocation.stdin === false || invocation.stdin === 'none') {
    return undefined;
  }
  return JSON.stringify(request);
}

function captureInvocationResult(outcome, invocation, result) {
  const metadata = optionalObject(outcome.metadata);
  const invocationResult = {
    command: invocation.command,
    argv: invocation.argv || [],
    cwd: invocation.cwd || '',
    exit_status: result.status ?? 0,
    signal: result.signal || null,
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
    ? { stop: true, reason: 'max_iterations_reached' }
    : { stop: false };
}

function collectEvidence({ iteration, outcome, result, artifacts }) {
  const resultEvidence = result === outcome ? [] : normalizeArray(result?.evidence_refs || result?.evidence);
  return [
    ...normalizeArray(artifacts).map((artifact) => ({
      kind: 'artifact',
      iteration,
      ref: artifact.path || artifact.url || artifact.id || artifact.name || '',
      artifact,
    })),
    ...normalizeArray(outcome?.evidence_refs || outcome?.evidence).map((ref) => ({
      kind: ref.kind || 'evidence_ref',
      iteration,
      ref: evidenceRefUrl(ref),
      evidence: ref,
    })),
    ...resultEvidence.map((ref) => ({
      kind: ref.kind || 'evidence_ref',
      iteration,
      ref: evidenceRefUrl(ref),
      evidence: ref,
    })),
  ];
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
  if (successStatus === 'pr_opened' || completionOutcomeSatisfied || (['no_changes', 'no_op'].includes(successStatus) && noChangesAllowed)) {
    return assertion;
  }
  throw new Error(`scenario ${assertion.scenario_id} expected opened PR, satisfied completion outcome, or allowed no-changes result, got job_status=${jobStatus} success_status=${successStatus} completion_outcome_satisfied=${completionOutcomeSatisfied ? 'true' : 'false'} no_changes_allowed=${noChangesAllowed ? 'true' : 'false'}`);
}

function buildBoundedProductionProof(options = {}) {
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

module.exports = {
  assertGenericAgentLoopOutcome,
  assertGenericAgentLoopRuntimeContract,
  buildGenericAgentLoopRequest,
  materializeGenericAgentLoopResults,
  runGenericAgentLoop,
  runGenericDeterministicLoop,
  validateGenericAgentLoopOutcomeContract,
  writeGenericAgentLoopArtifacts,
};
