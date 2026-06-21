'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { runDeterministicLoop } = require('./deterministic-loop-runner');
const { runtimeAgentCiTaskExecutorConfig } = require('./runtime-agent-ci-plan');

function buildGenericAgentLoopRequest(options = {}) {
  const plan = requiredObject(options.plan, 'plan');
  const runtime = requiredObject(options.runtime, 'runtime');
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
  const loop = runDeterministicLoop({
    loopId: request.task_id,
    maxIterations: 1,
    state: { request },
    buildIteration: ({ state }) => state.request,
    execute: ({ input }) => normalizeOutcome(execute({ ...options, request: input, runtime }), input),
    reconcile: ({ state }) => state,
    stopCriteria: () => true,
  });
  const outcome = loop.iterations[0]?.outcome || normalizeOutcome(null, request);
  const results = materializeGenericAgentLoopResults(outcome, { ...options, runtime });
  const assertion = options.validate === false ? null : assertGenericAgentLoopOutcome(results, options.validationPolicy || options.validation_policy || {});
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

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function nonEmpty(value) {
  return value !== undefined && value !== null && value !== '';
}

module.exports = {
  assertGenericAgentLoopOutcome,
  buildGenericAgentLoopRequest,
  materializeGenericAgentLoopResults,
  runGenericAgentLoop,
  runDeterministicLoop,
  writeGenericAgentLoopArtifacts,
};
