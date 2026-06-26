'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
  buildGenericAgentLoopRequest,
  runGenericAgentLoop,
  runGenericDeterministicLoop,
  writeGenericAgentLoopArtifacts,
} = require('./generic-agent-loop-runner');
const {
  loopPolicyMaxRevolutions,
  normalizeLoopPolicy: normalizeSharedLoopPolicy,
} = require('./loop-policy');
const { executeFanoutReconcileRun } = require('./fanout-reconcile-runner');
const { resolveRuntimeProvider, runtimeIdFromOptions } = require('./runtime-provider-resolver.cjs');
const { runtimeAgentArtifactPaths } = require('./artifact-paths.cjs');

async function runHeadlessDeterministicLoop(options = {}) {
  const spec = requiredObject(options.spec || options.config || options.plan, 'spec');
  const repoRoot = options.repoRoot || options.repo_root || path.resolve(__dirname, '..', '..');
  const runtime = resolveLoopRuntime(spec, { ...options, repoRoot });
  const tasks = loopTasks(spec);
  const events = [];
  const dryRun = options.dryRun === true || options.dry_run === true || spec.dry_run === true;
  const validate = options.validate !== false && spec.validate !== false && !dryRun;
  const startedAt = new Date().toISOString();
  const taskResults = [];
  let finalOutcome = null;
  let finalResults = null;

  pushEvent(events, 'loop_started', { loop_id: loopId(spec), task_count: tasks.length, dry_run: dryRun });

  const fanoutRun = await executeFanoutReconcileRun({
    artifact_paths: runtimeAgentArtifactPaths({ ...options, ...spec }),
    plan: {
      schema: 'homeboy/headless-deterministic-loop-task-plan/v1',
      summary: { task_count: tasks.length },
      task_requests: tasks.map((task, index) => ({ ...task, task_index: index })),
    },
    concurrency: spec.concurrency || spec.task_concurrency || spec.max_concurrency || options.concurrency,
    task_id: (task) => task.task_id || task.workload_id || `task-${task.task_index + 1}`,
    task_order: (record) => record.task_index,
    execute_task_request: (task) => executeHeadlessTask({
      ...options,
      task,
      spec,
      runtime,
      repoRoot,
      dryRun,
      validate,
      startedAt,
      events,
    }),
    is_record_successful: (record) => record.status === 'completed',
    classify_outcome: (record) => record.outcome,
    include_reconciliation: false,
  });

  for (const record of fanoutRun.records) {
    if (!record.task_result) {
      throw new Error(record.error_message || `headless task ${record.id || '(unknown)'} failed before producing a result`);
    }
    taskResults.push(record.task_result);
  }

  finalOutcome = taskResults.at(-1)?.outcome || null;
  finalResults = taskResults.at(-1)?.results || null;

  const completedAt = new Date().toISOString();
  const result = {
    schema: 'homeboy/headless-deterministic-loop-result/v1',
    loop_id: loopId(spec),
    status: loopStatus(taskResults),
    dry_run: dryRun,
    started_at: startedAt,
    completed_at: completedAt,
    runtime: { id: runtime.id, backend: runtime.executor.backend },
    tasks: taskResults,
    outcome: finalOutcome,
    results: finalResults,
    state: taskResults.at(-1)?.state || null,
    fanout: {
      schema: fanoutRun.schema,
      status: fanoutRun.status,
      summary: fanoutRun.summary,
      records: fanoutRun.records.map((record) => ({
        id: record.id,
        task_index: record.task_index,
        status: record.status,
        outcome_status: record.outcome?.status || '',
      })),
    },
    events,
  };
  pushEvent(events, 'loop_completed', { loop_id: result.loop_id, status: result.status });
  return result;
}

function executeHeadlessTask(options = {}) {
  const { task, spec, runtime, repoRoot, dryRun, validate, events } = options;
  const plan = { ...spec, ...task, dry_run: dryRun };
  let finalOutcome = null;
  let finalResults = null;
  let finalLoop = null;
  const controllerExecution = normalizeControllerExecution(plan.controller_execution || plan.controllerExecution || plan.controller);
  if (controllerExecution) {
    const request = buildControllerExecutionRequest(plan, controllerExecution, { repoRoot });
    pushEvent(events, 'task_planned', { task_id: request.task_id, backend: 'homeboy-controller' });
    if (dryRun) {
      finalOutcome = dryRunOutcome(request);
      finalResults = dryRunResults(request);
      pushEvent(events, 'task_dry_run', { task_id: request.task_id });
    } else {
      pushEvent(events, 'task_started', { task_id: request.task_id });
      const result = executeControllerExecution({ ...options, plan, request, controllerExecution, repoRoot });
      finalOutcome = result.outcome;
      finalResults = result.results;
      finalLoop = result.loop;
      pushEvent(events, 'task_completed', { task_id: request.task_id, status: result.outcome.status });
    }

    const taskResult = {
      task_id: request.task_id,
      request,
      outcome: finalOutcome,
      results: finalResults,
      loop: finalLoop,
      loop_policy: finalLoop?.policy_status || null,
      state: finalLoop?.state || null,
    };
    return {
      id: request.task_id,
      task_index: task.task_index,
      status: ['succeeded', 'no_op'].includes(finalOutcome?.status) ? 'completed' : 'failed',
      outcome: finalOutcome,
      task_result: taskResult,
    };
  }

  const request = buildGenericAgentLoopRequest({
    plan,
    runtime,
    configPath: options.configPath || options.config_path || '',
    extensionPath: options.extensionPath || options.extension_path || spec.homeboy_extensions_path || repoRoot,
    replayBundleDir: options.replayBundleDir || options.replay_bundle_dir || spec.replay_bundle_dir,
    env: options.env,
  });
  pushEvent(events, 'task_planned', { task_id: request.task_id, backend: request.executor.backend });

  const loopPolicy = normalizeLoopPolicy(plan);
  assertHeadlessPolicyModeSupported(loopPolicy, options);

  if (dryRun) {
    finalOutcome = dryRunOutcome(request);
    finalResults = dryRunResults(request);
    pushEvent(events, 'task_dry_run', { task_id: request.task_id });
  } else if (loopPolicy.enabled) {
    pushEvent(events, 'task_policy_started', { task_id: request.task_id, max_iterations: loopPolicy.max_iterations });
    const result = runHeadlessPolicyLoop({
      ...options,
      plan,
      request,
      runtime,
      repoRoot,
      configPath: options.configPath || options.config_path || '',
      validate,
      validationPolicy: validationPolicyForPlan(plan, options.validationPolicy || options.validation_policy),
      loopPolicy,
      extensionPath: options.extensionPath || options.extension_path || spec.homeboy_extensions_path || repoRoot,
      replayBundleDir: options.replayBundleDir || options.replay_bundle_dir || spec.replay_bundle_dir,
    });
    finalOutcome = result.outcome;
    finalResults = result.results;
    finalLoop = result.loop;
    pushEvent(events, 'task_policy_completed', { task_id: request.task_id, status: result.outcome.status, stop_reason: result.loop.policy_status.stop_reason });
  } else {
    pushEvent(events, 'task_started', { task_id: request.task_id });
    const result = runGenericAgentLoop({
      ...options,
      plan,
      request,
      runtime,
      repoRoot,
      configPath: options.configPath || options.config_path || '',
      validate,
      validationPolicy: validationPolicyForPlan(plan, options.validationPolicy || options.validation_policy),
    });
    finalOutcome = result.outcome;
    finalResults = result.results;
    finalLoop = result.loop;
    pushEvent(events, 'task_completed', { task_id: request.task_id, status: result.outcome.status });
    if (result.assertion) {
      pushEvent(events, 'task_asserted', { task_id: request.task_id, assertion: result.assertion });
    }
  }

  const taskResult = {
    task_id: request.task_id,
    request,
    outcome: finalOutcome,
    results: finalResults,
    loop: finalLoop,
    loop_policy: finalLoop?.policy_status || null,
    state: finalLoop?.state || null,
  };
  return {
    id: request.task_id,
    task_index: task.task_index,
    status: ['succeeded', 'no_op'].includes(finalOutcome?.status) ? 'completed' : 'failed',
    outcome: finalOutcome,
    task_result: taskResult,
  };
}

function normalizeControllerExecution(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return null;
  }
  const spec = stringValue(value.spec || value.spec_path || value.specPath || value.controller_spec || value.controllerSpec || value.path);
  if (!spec) {
    return null;
  }
  return {
    schema: value.schema || 'homeboy/headless-controller-execution/v1',
    spec,
    inputs: stringValue(value.inputs || value.inputs_path || value.inputsPath),
    policy_result: stringValue(value.policy_result || value.policyResult || value.policy_result_path || value.policyResultPath),
    output: stringValue(value.output || value.output_path || value.outputPath),
    max_actions: positiveInteger(value.max_actions || value.maxActions) || 100,
    prepare: normalizeArray(value.prepare || value.prepare_commands || value.prepareCommands),
    env: optionalObject(value.env),
    metadata: optionalObject(value.metadata),
  };
}

function buildControllerExecutionRequest(plan, controllerExecution, options = {}) {
  const taskId = plan.task_id || plan.workload_id || 'headless-controller';
  const cwd = resolveControllerCwd(plan, options);
  return {
    schema: 'homeboy/headless-controller-execution-request/v1',
    task_id: String(taskId),
    group_key: plan.group_key || plan.workload_id || '',
    instructions: plan.prompt || plan.workload_label || 'Run Homeboy controller task.',
    workspace: compactObject({ repository: plan.target_repo, path: plan.component_path }),
    controller_execution: controllerExecution,
    cwd,
    artifact_declarations: normalizeArray(plan.artifact_declarations),
    expected_artifacts: expectedArtifactsFromPlan(plan),
  };
}

function executeControllerExecution(options = {}) {
  const request = requiredObject(options.request, 'request');
  const controllerExecution = requiredObject(options.controllerExecution || options.controller_execution, 'controllerExecution');
  const executeController = options.executeController || options.execute_controller || defaultExecuteControllerExecution;
  const result = executeController({ ...options, request, controllerExecution });
  const status = result?.status === 'succeeded' || result?.success === true ? 'succeeded' : 'failed';
  const summary = result?.summary || (status === 'succeeded' ? 'Homeboy controller execution succeeded.' : 'Homeboy controller execution failed.');
  const outcome = {
    schema: 'homeboy/agent-task-outcome/v1',
    task_id: request.task_id,
    status,
    summary,
    metadata: {
      controller_execution: controllerExecution,
      controller_result: result?.result || result || {},
      results: result?.results || controllerResultsScenario(request, status, result),
    },
  };
  return {
    outcome,
    results: outcome.metadata.results,
    loop: null,
  };
}

function defaultExecuteControllerExecution(options = {}) {
  const controllerExecution = requiredObject(options.controllerExecution || options.controller_execution, 'controllerExecution');
  const cwd = options.request?.cwd || resolveControllerCwd(options.plan || {}, options);
  const env = { ...process.env, ...controllerExecution.env };
  for (const command of controllerExecution.prepare) {
    runControllerCommand(command, { cwd, env });
  }
  const homeboyBin = options.homeboyBin || options.homeboy_bin || process.env.HOMEBOY_BIN || 'homeboy';
  const args = ['agent-task', 'controller', 'run-from-spec', specArg(controllerExecution.spec), '--max-actions', String(controllerExecution.max_actions)];
  if (controllerExecution.inputs) {
    args.push('--inputs', specArg(controllerExecution.inputs));
  }
  if (controllerExecution.policy_result) {
    args.push('--policy-result', specArg(controllerExecution.policy_result));
  }
  if (controllerExecution.output) {
    args.push('--output', controllerExecution.output);
  }
  const run = spawnSync(homeboyBin, args, { cwd, env, encoding: 'utf8' });
  if (run.status !== 0) {
    return {
      status: 'failed',
      summary: `Homeboy controller exited with status ${run.status}.`,
      command: [homeboyBin, ...args],
      stdout: run.stdout || '',
      stderr: run.stderr || '',
    };
  }
  const parsed = controllerExecution.output ? readJsonOrNull(path.resolve(cwd, controllerExecution.output)) : null;
  return {
    status: 'succeeded',
    summary: 'Homeboy controller execution succeeded.',
    command: [homeboyBin, ...args],
    stdout: run.stdout || '',
    stderr: run.stderr || '',
    result: parsed,
  };
}

function resolveControllerCwd(plan = {}, options = {}) {
  const componentPath = stringValue(plan.component_path || plan.componentPath || plan.workspace?.path);
  const base = options.workloadRoot || options.workload_root || process.cwd();
  if (!componentPath || componentPath === '.') {
    return base;
  }
  return path.isAbsolute(componentPath) ? componentPath : path.resolve(base, componentPath);
}

function runControllerCommand(command, options = {}) {
  const normalized = Array.isArray(command) ? { argv: command } : command;
  if (!normalized || typeof normalized !== 'object' || !Array.isArray(normalized.argv) || normalized.argv.length === 0) {
    throw new Error('controller_execution prepare commands must include a non-empty argv array.');
  }
  const [bin, ...args] = normalized.argv;
  const run = spawnSync(bin, args, {
    cwd: normalized.cwd ? path.resolve(options.cwd || process.cwd(), normalized.cwd) : options.cwd,
    env: { ...(options.env || process.env), ...optionalObject(normalized.env) },
    encoding: 'utf8',
  });
  if (run.status !== 0) {
    throw new Error(`controller_execution prepare command failed: ${[bin, ...args].join(' ')}\n${run.stderr || run.stdout || ''}`);
  }
}

function specArg(value) {
  return value.startsWith('@') ? value : `@${value}`;
}

function controllerResultsScenario(request, status, result) {
  return {
    scenarios: [{
      id: request.task_id,
      metrics: { homeboy_controller_execution_mean: status === 'succeeded' ? 1 : 0 },
      metadata: {
        job_status: status,
        success_status: status,
        completion_outcome: status,
        completion_outcome_satisfied: status === 'succeeded',
        controller_result_status: result?.status || '',
      },
    }],
  };
}

function readJsonOrNull(filePath) {
  try {
    return JSON.parse(fs.readFileSync(filePath, 'utf8'));
  } catch {
    return null;
  }
}

function writeHeadlessDeterministicLoopArtifacts(options = {}) {
  const artifactPaths = runtimeAgentArtifactPaths(options);
  if (artifactPaths.loop_result) {
    writeJsonFile(artifactPaths.loop_result, options.result);
  }
  if (artifactPaths.events) {
    writeJsonFile(artifactPaths.events, options.result?.events || []);
  }
  if (artifactPaths.loop_policy) {
    writeJsonFile(artifactPaths.loop_policy, policyArtifact(options.result));
  }
  if (artifactPaths.status) {
    writeJsonFile(artifactPaths.status, statusArtifact(options.result));
  }
  writeGenericAgentLoopArtifacts({
    outcome: options.result?.outcome,
    results: options.result?.results,
    artifact_paths: { ...artifactPaths, status: '' },
  });
}

function runHeadlessPolicyLoop(options = {}) {
  const loopPolicy = requiredObject(options.loopPolicy || options.loop_policy, 'loopPolicy');
  const basePlan = requiredObject(options.plan, 'plan');
  const baseRequest = requiredObject(options.request, 'request');
  const runtime = requiredObject(options.runtime, 'runtime');
  const iterationRecords = [];
  let latestAccepted = false;
  let stopReason = '';

  const loop = runGenericDeterministicLoop({
    loopId: `${baseRequest.task_id}-headless-policy`,
    mode: loopPolicy.mode,
    maxRevolutions: loopPolicy.max_revolutions,
    maxIterations: loopPolicy.max_iterations,
    maxSynchronousRevolutions: loopPolicy.max_iterations,
    durationMs: loopPolicy.duration_ms,
    deadlineAt: loopPolicy.deadline_at,
    state: {
      request: baseRequest,
      plan: basePlan,
      repair_required: false,
      failure_summary: '',
    },
    buildTask: ({ iteration, state }) => {
      const candidatePlan = iteration === 1 || !state.repair_required
        ? basePlan
        : materializeRepairPlan(loopPolicy.repair_task_template, basePlan, { iteration, state, baseRequest });
      const candidateRequest = buildRequestForPlan(candidatePlan, options);
      const validationPlan = loopPolicy.validation_task
        ? materializeTemplate(loopPolicy.validation_task, basePlan, { iteration, state, baseRequest, candidateRequest })
        : null;
      const validationRequest = validationPlan ? buildRequestForPlan(validationPlan, options) : null;
      return {
        schema: 'homeboy/headless-loop-policy-iteration-task/v1',
        iteration,
        candidate_plan: candidatePlan,
        candidate_request: candidateRequest,
        validation_plan: validationPlan,
        validation_request: validationRequest,
      };
    },
    executeTask: ({ task }) => {
      const candidate = runGenericAgentLoop({
        ...options,
        plan: task.candidate_plan,
        request: task.candidate_request,
        runtime,
        validate: false,
        maxIterations: 1,
      });
      if (!task.validation_request) {
        return policyIterationOutcome(task, candidate, null);
      }
      const validation = runGenericAgentLoop({
        ...options,
        plan: task.validation_plan,
        request: task.validation_request,
        runtime,
        validate: false,
        maxIterations: 1,
      });
      return policyIterationOutcome(task, candidate, validation);
    },
    collectResult: ({ outcome }) => outcome,
    reconcile: ({ state, task, result }) => {
      const accepted = policyAcceptsResult(result, loopPolicy);
      const failureSummary = accepted ? '' : failureSummaryForResult(result);
      const repair = !accepted && loopPolicy.repair_task_template
        ? {
            required: true,
            task_template: loopPolicy.repair_task_template,
            next_task_id: materializeRepairPlan(loopPolicy.repair_task_template, basePlan, {
              iteration: task.iteration + 1,
              state: { ...state, failure_summary: failureSummary },
              baseRequest,
            }).task_id,
          }
        : { required: false };
      const record = {
        schema: 'homeboy/headless-loop-policy-iteration/v1',
        loop_id: `${baseRequest.task_id}-headless-policy`,
        iteration: task.iteration,
        candidate_task_id: task.candidate_request.task_id,
        validation_task_id: task.validation_request?.task_id || '',
        accepted,
        result: compactOutcome(result),
        repair,
      };
      iterationRecords.push(record);
      latestAccepted = accepted;
      return {
        ...state,
        latest_result: result,
        latest_policy_iteration: record,
        repair_required: repair.required,
        failure_summary: failureSummary,
        accepted,
        policy_iterations: iterationRecords,
      };
    },
    stopPolicy: (context) => {
      const stop = firstMatchingCondition(loopPolicy.stop_conditions, context);
      if (stop) {
        stopReason = stop.reason;
        return { stop: true, reason: stopReason, data: stop.condition };
      }
      if (latestAccepted) {
        stopReason = 'accepted';
        return { stop: true, reason: stopReason };
      }
      if (context.iteration >= loopPolicy.max_iterations) {
        stopReason = 'max_revolutions_reached';
        return { stop: true, reason: stopReason };
      }
      return { stop: false };
    },
    shouldContinue: (context) => {
      if (latestAccepted || context.iteration >= loopPolicy.max_iterations) {
        return false;
      }
      if (loopPolicy.continue_conditions.length === 0) {
        return true;
      }
      const matched = firstMatchingCondition(loopPolicy.continue_conditions, context);
      return matched ? { continue: true, reason: matched.reason } : { continue: false, reason: 'continue_conditions_not_satisfied' };
    },
  });

  const policyStatus = {
    schema: 'homeboy/headless-loop-policy-status/v1',
    status: latestAccepted ? 'succeeded' : 'failed',
    stop_reason: stopReason || loop.iterations.at(-1)?.stop?.reason || loop.stop?.reason || 'unknown',
    max_iterations: loopPolicy.max_iterations,
    max_revolutions: loopPolicy.max_revolutions,
    mode: loopPolicy.mode,
    iteration_count: iterationRecords.length,
    accepted: latestAccepted,
    iterations: iterationRecords,
  };
  loop.policy_status = policyStatus;
  return {
    request: baseRequest,
    outcome: policyOutcome(baseRequest, policyStatus),
    results: policyResults(baseRequest, policyStatus),
    assertion: null,
    loop,
  };
}

function compactOutcome(outcome) {
  return {
    schema: outcome?.schema || '',
    task_id: outcome?.task_id || '',
    status: outcome?.status || '',
    summary: outcome?.summary || '',
    diagnostics: normalizeArray(outcome?.diagnostics),
    artifacts: normalizeArray(outcome?.artifacts),
    evidence_refs: normalizeArray(outcome?.evidence_refs || outcome?.evidence),
  };
}

function policyIterationOutcome(task, candidate, validation) {
  const result = validation?.outcome || candidate.outcome;
  return {
    ...result,
    metadata: {
      ...optionalObject(result?.metadata),
      headless_loop_policy: {
        candidate_task_id: task.candidate_request.task_id,
        validation_task_id: task.validation_request?.task_id || '',
        candidate_outcome: compactOutcome(candidate.outcome),
        validation_outcome: validation?.outcome ? compactOutcome(validation.outcome) : null,
      },
    },
  };
}

function normalizeLoopPolicy(plan) {
  const raw = optionalObject(plan.loop_policy || plan.loopPolicy);
  const primitive = normalizeSharedLoopPolicy({ ...plan, ...raw }, { defaultMode: 'count', defaultMaxRevolutions: 1 });
  const maxIterations = primitive.mode === 'count'
    ? loopPolicyMaxRevolutions(primitive)
    : positiveInteger(raw.max_synchronous_revolutions || raw.maxSynchronousRevolutions || plan.max_synchronous_revolutions || plan.maxSynchronousRevolutions) || 0;
  const enabled = Object.keys(raw).length > 0 || maxIterations > 1;
  return {
    enabled,
    mode: primitive.mode,
    max_iterations: maxIterations || 1,
    max_revolutions: primitive.max_revolutions,
    max_synchronous_revolutions: primitive.max_synchronous_revolutions,
    duration_ms: primitive.duration_ms,
    deadline_at: primitive.deadline_at,
    accepted_statuses: normalizeArray(raw.accepted_statuses || raw.acceptedStatuses).length > 0
      ? normalizeArray(raw.accepted_statuses || raw.acceptedStatuses)
      : ['accepted', 'succeeded', 'passed', 'no_op'],
    continue_conditions: normalizeArray(raw.continue_conditions || raw.continueConditions),
    stop_conditions: normalizeArray(raw.stop_conditions || raw.stopConditions),
    validation_task: optionalNullableObject(raw.validation_task || raw.validationTask),
    repair_task_template: optionalNullableObject(raw.repair_task_template || raw.repairTaskTemplate),
  };
}

function assertHeadlessPolicyModeSupported(loopPolicy, options = {}) {
  if (!loopPolicy.enabled || loopPolicy.mode === 'count') {
    return;
  }
  if (typeof options.submitIteration === 'function' && typeof options.pollIteration === 'function') {
    throw new Error('Headless durable duration and indefinite loop policies are not implemented yet; submit/poll support must route through createDurableDeterministicLoop before enabling this mode.');
  }
  throw new Error('Headless duration and indefinite loop policies require a durable submit/poll implementation. Use count mode for synchronous headless loops.');
}

function buildRequestForPlan(plan, options) {
  return buildGenericAgentLoopRequest({
    plan,
    runtime: options.runtime,
    configPath: options.configPath || options.config_path || '',
    extensionPath: options.extensionPath || options.extension_path,
    replayBundleDir: options.replayBundleDir || options.replay_bundle_dir,
    env: options.env,
  });
}

function materializeRepairPlan(template, basePlan, context) {
  const repairTemplate = template || basePlan;
  return {
    ...basePlan,
    ...materializeTemplate(repairTemplate, basePlan, context),
  };
}

function materializeTemplate(template, basePlan, context) {
  return replaceTemplateStrings({ ...basePlan, ...template }, {
    task_id: basePlan.task_id || basePlan.workload_id || context.baseRequest?.task_id || '',
    workload_id: basePlan.workload_id || basePlan.task_id || context.baseRequest?.task_id || '',
    iteration: context.iteration,
    previous_task_id: context.state?.latest_policy_iteration?.candidate_task_id || context.baseRequest?.task_id || '',
    failure_summary: context.state?.failure_summary || '',
  });
}

function replaceTemplateStrings(value, vars) {
  if (typeof value === 'string') {
    return value.replace(/\{\{\s*([a-zA-Z0-9_]+)\s*\}\}/g, (match, key) => Object.prototype.hasOwnProperty.call(vars, key) ? String(vars[key]) : match);
  }
  if (Array.isArray(value)) {
    return value.map((entry) => replaceTemplateStrings(entry, vars));
  }
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, replaceTemplateStrings(entry, vars)]));
  }
  return value;
}

function policyAcceptsResult(result, policy) {
  return policy.accepted_statuses.includes(result?.status);
}

function firstMatchingCondition(conditions, context) {
  for (const condition of conditions) {
    if (conditionMatches(condition, context)) {
      return { reason: condition.reason || condition.name || 'condition_satisfied', condition };
    }
  }
  return null;
}

function conditionMatches(condition, context) {
  if (!condition || typeof condition !== 'object' || Array.isArray(condition)) {
    return false;
  }
  const value = condition.path ? getPath(context, condition.path) : undefined;
  if (Object.prototype.hasOwnProperty.call(condition, 'equals')) {
    return value === condition.equals;
  }
  if (Array.isArray(condition.in)) {
    return condition.in.includes(value);
  }
  if (Array.isArray(condition.outcome_status)) {
    return condition.outcome_status.includes(context.result?.status || context.outcome?.status);
  }
  if (condition.outcome_status) {
    return (context.result?.status || context.outcome?.status) === condition.outcome_status;
  }
  return false;
}

function getPath(value, pathExpression) {
  return String(pathExpression).split('.').filter(Boolean).reduce((current, segment) => {
    if (!current || typeof current !== 'object') {
      return undefined;
    }
    return current[segment];
  }, value);
}

function failureSummaryForResult(result) {
  return result?.summary || result?.diagnostics?.[0]?.message || result?.status || 'iteration not accepted';
}

function policyOutcome(request, status) {
  return {
    schema: 'homeboy/agent-task-outcome/v1',
    task_id: request.task_id,
    status: status.status,
    summary: `Headless loop policy ${status.status} after ${status.iteration_count} iteration(s).`,
    metadata: { headless_loop_policy_status: status },
  };
}

function policyResults(request, status) {
  return {
    scenarios: [{
      id: request.task_id,
      label: request.instructions || request.task_id,
      metrics: { generic_agent_task_executor_mean: status.status === 'succeeded' ? 1 : 0 },
      metadata: {
        job_status: status.status,
        success_status: status.status,
        completion_outcome: status.stop_reason,
        completion_outcome_satisfied: status.status === 'succeeded',
        headless_loop_policy_status: status,
      },
    }],
  };
}

function policyArtifact(result) {
  return {
    schema: 'homeboy/headless-loop-policy-artifact/v1',
    loop_id: result?.loop_id || '',
    status: result?.status || '',
    tasks: normalizeArray(result?.tasks).map((task) => ({
      task_id: task.task_id,
      loop_policy: task.loop_policy,
    })),
  };
}

function statusArtifact(result) {
  return {
    schema: 'homeboy/headless-deterministic-loop-status/v1',
    loop_id: result?.loop_id || '',
    status: result?.status || '',
    task_count: normalizeArray(result?.tasks).length,
    failed_tasks: normalizeArray(result?.tasks).filter((task) => !['succeeded', 'no_op'].includes(task.outcome?.status)).map((task) => task.task_id),
  };
}

function resolveLoopRuntime(spec, options = {}) {
  if (options.runtime) {
    return options.runtime;
  }
  if (spec.runtime_manifest) {
    const manifest = requiredObject(spec.runtime_manifest, 'runtime_manifest');
    const id = manifest.id || spec.runtime_id;
    return resolveRuntimeProvider(id, {
      ...options,
      registry: { [id]: manifest },
      workspace: spec.component_path || options.workspace || process.cwd(),
    });
  }
  return resolveRuntimeProvider(runtimeIdFromOptions({ runtime_id: spec.runtime_id || spec.runtime }, process.env), {
    ...options,
    workspace: spec.component_path || options.workspace || process.cwd(),
  });
}

function loopTasks(spec) {
  const tasks = Array.isArray(spec.tasks) && spec.tasks.length > 0 ? spec.tasks : [spec];
  return tasks.map((task, index) => {
    const normalized = requiredObject(task, `tasks[${index}]`);
    return {
      ...normalized,
      task_id: normalized.task_id || normalized.workload_id || `${loopId(spec)}-${index + 1}`,
      workload_id: normalized.workload_id || normalized.task_id || `${loopId(spec)}-${index + 1}`,
    };
  });
}

function dryRunOutcome(request) {
  return {
    schema: 'homeboy/agent-task-outcome/v1',
    task_id: request.task_id,
    status: 'no_op',
    summary: 'Headless deterministic loop dry run materialized the task request.',
    metadata: { request },
  };
}

function dryRunResults(request) {
  return {
    scenarios: [{
      id: request.task_id,
      label: request.instructions || request.task_id,
      metrics: { generic_agent_task_executor_mean: 1 },
      metadata: {
        job_status: 'completed',
        success_status: 'no_changes',
        completion_outcome: 'dry_run',
        completion_outcome_satisfied: true,
      },
    }],
  };
}

function validationPolicyForPlan(plan, policy = {}) {
  return {
    ...policy,
    scenario_id: policy.scenario_id || plan.workload_id || plan.task_id,
    success_requires_pr: policy.success_requires_pr ?? plan.success_requires_pr,
    success_completion_outcomes: policy.success_completion_outcomes || plan.success_completion_outcomes,
  };
}

function loopStatus(taskResults) {
  return taskResults.every((entry) => ['succeeded', 'no_op'].includes(entry.outcome?.status)) ? 'succeeded' : 'failed';
}

function positiveInteger(value) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 0;
}

function optionalObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function compactObject(value) {
  return Object.fromEntries(Object.entries(value || {}).filter(([, entry]) => entry !== undefined && entry !== null && entry !== ''));
}

function optionalNullableObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : null;
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function stringValue(value) {
  return typeof value === 'string' && value.trim() !== '' ? value.trim() : '';
}

function expectedArtifactsFromPlan(plan) {
  const declared = normalizeArray(plan.artifact_declarations)
    .filter((artifact) => artifact && typeof artifact === 'object' && artifact.required === true && artifact.name)
    .map((artifact) => artifact.name);
  return normalizeArray(plan.expected_artifacts || plan.expectedArtifacts).length > 0
    ? normalizeArray(plan.expected_artifacts || plan.expectedArtifacts)
    : declared;
}

function pushEvent(events, type, data = {}) {
  events.push({
    schema: 'homeboy/headless-deterministic-loop-event/v1',
    sequence: events.length + 1,
    type,
    timestamp: new Date().toISOString(),
    ...data,
  });
}

function loopId(spec) {
  return spec.loop_id || spec.plan_id || spec.workload_id || 'headless-deterministic-loop';
}

function writeJsonFile(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${safeJsonStringify(value)}\n`);
}

function safeJsonStringify(value) {
  const seen = new WeakSet();
  return JSON.stringify(value, (key, entry) => {
    if (entry && typeof entry === 'object') {
      if (seen.has(entry)) {
        return '[Circular]';
      }
      seen.add(entry);
    }
    return entry;
  }, 2);
}

function requiredObject(value, name) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${name} must be an object.`);
  }
  return value;
}

module.exports = {
  runHeadlessDeterministicLoop,
  writeHeadlessDeterministicLoopArtifacts,
};
