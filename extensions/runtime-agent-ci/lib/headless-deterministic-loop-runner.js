'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {
  buildGenericAgentLoopRequest,
  runGenericAgentLoop,
  writeGenericAgentLoopArtifacts,
} = require('./generic-agent-loop-runner');
const { resolveRuntimeProvider } = require('./runtime-provider-resolver.cjs');

function runHeadlessDeterministicLoop(options = {}) {
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

  for (const task of tasks) {
    const plan = { ...spec, ...task, dry_run: dryRun };
    let finalLoop = null;
    const request = buildGenericAgentLoopRequest({
      plan,
      runtime,
      configPath: options.configPath || options.config_path || '',
      extensionPath: options.extensionPath || options.extension_path || spec.homeboy_extensions_path || repoRoot,
      replayBundleDir: options.replayBundleDir || options.replay_bundle_dir || spec.replay_bundle_dir,
      env: options.env,
    });
    pushEvent(events, 'task_planned', { task_id: request.task_id, backend: request.executor.backend });

    if (dryRun) {
      finalOutcome = dryRunOutcome(request);
      finalResults = dryRunResults(request);
      pushEvent(events, 'task_dry_run', { task_id: request.task_id });
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

    taskResults.push({
      task_id: request.task_id,
      request,
      outcome: finalOutcome,
      results: finalResults,
      loop: finalLoop,
      state: finalLoop?.state || null,
    });
  }

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
    events,
  };
  pushEvent(events, 'loop_completed', { loop_id: result.loop_id, status: result.status });
  return result;
}

function writeHeadlessDeterministicLoopArtifacts(options = {}) {
  if (options.loopResultFile || options.loop_result_file) {
    writeJsonFile(options.loopResultFile || options.loop_result_file, options.result);
  }
  if (options.eventsFile || options.events_file) {
    writeJsonFile(options.eventsFile || options.events_file, options.result?.events || []);
  }
  writeGenericAgentLoopArtifacts({
    outcome: options.result?.outcome,
    results: options.result?.results,
    outcomeFile: options.outcomeFile || options.outcome_file,
    resultsFile: options.resultsFile || options.results_file,
  });
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
  return resolveRuntimeProvider(spec.runtime_id || process.env.RUNTIME || process.env.RUNTIME_PROVIDER || process.env.BACKEND, {
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
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
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
