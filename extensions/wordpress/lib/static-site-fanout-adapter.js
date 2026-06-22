'use strict';

/* eslint-disable camelcase */

/**
 * Internal dependencies
 */
const {
  createFanoutReconcilePlan,
  executeFanoutReconcileRun,
  groupFanoutItems,
} = require('../../runtime-agent-ci/lib/fanout-reconcile-runner');

const PLAN_SCHEMA = 'homeboy/static-site-fanout-plan/v1';
const RUN_SCHEMA = 'homeboy/static-site-fanout-run/v1';
const RECONCILIATION_SCHEMA = 'homeboy/static-site-fanout-reconciliation/v1';
const AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const CODEBOX_COMPATIBILITY_TASK_SCHEMA = 'wp-codebox/task-input/v1';
const DEFAULT_PRESET = 'static-site/import-validation';
const COMPATIBILITY_AGENT_TASK_BACKEND = 'codebox';
const CODEBOX_COMPATIBILITY_PROVIDER = 'wp-codebox';
const DEFAULT_AGENT_TASK_PRESET = {
  runtime_task: 'static-site/import-validation',
};

function createStaticSiteFanoutPlan(input = {}) {
  const findings = normalizeFindings(input.findings || input.finding_artifacts || input.finding_packets || input.packets || []);
  const groups = normalizeFanoutGroups(input.groups || input.fanout_groups || input.controller?.fanout_groups, findings, input);
  const orchestrator = normalizeOrchestrator(input);
  const requestKind = input.request_kind || input.requestKind || 'agent-task';
  const plan = createFanoutReconcilePlan({
    schema: PLAN_SCHEMA,
    orchestrator,
    groups,
    summary: {
      preset: orchestrator.preset,
      finding_count: groups.reduce((count, group) => count + group.items.length, 0),
      no_actionable_findings: groups.length === 0,
    },
    render_task_request: (group) => createTaskRequest(group, orchestrator, { ...input, request_kind: requestKind }),
    reconcile_plan: ({ groups: fanoutGroups, task_requests }) => staticSiteReconciliation({
      plan: { task_requests, summary: { group_count: fanoutGroups.length } },
      records: [],
      outcomes: [],
      groups: fanoutGroups,
    }),
  });

  return {
    ...plan,
    static_site: {
      preset: orchestrator.preset,
      finding_count: plan.summary.finding_count,
      group_count: groups.length,
      task_count: plan.task_requests.length,
      no_actionable_findings: groups.length === 0,
    },
  };
}

function normalizeOrchestrator(input) {
  const preset = input.preset || input.controller?.preset || DEFAULT_PRESET;
  return stripUndefined({
    id: input.orchestrator_id || input.orchestrator?.id || 'homeboy-extensions/static-site-fanout-adapter',
    run_id: input.run_id || input.orchestrator?.run_id || input.controller?.run_id || 'static-site-fanout-run',
    plan_id: input.plan_id || input.orchestrator?.plan_id || input.controller?.loop_id || 'static-site-fanout-plan',
    controller_id: input.controller_id || input.controller?.loop_id || input.controller?.id || '',
    source: input.source || input.orchestrator?.source || 'static-site/import-validation',
    preset,
    parent_plan_id: input.parent_plan_id || input.parentPlanId || input.orchestrator?.parent_plan_id || '',
    provider: input.provider || '',
    model: input.model || '',
    backend: input.backend || input.runtime_backend || input.runtimeBackend || input.agent_runtime_backend || input.agentRuntimeBackend || undefined,
    provider_plugin_paths: normalizeArray(input.provider_plugin_paths || input.providerPluginPaths),
    secret_env: normalizeArray(input.secret_env || input.secretEnv),
    compatibility_provider: codeboxCompatibilityRequested(input) ? CODEBOX_COMPATIBILITY_PROVIDER : undefined,
    request_schema: requestSchema(input),
  });
}

function normalizeFindings(findings) {
  return normalizeArray(findings).map((finding, index) => normalizeFinding(finding, index));
}

function normalizeFinding(finding, index) {
  const raw = finding && typeof finding === 'object' ? finding : { id: String(finding || `finding-${index + 1}`) };
  const id = text(raw.id) || text(raw.finding_id) || text(raw.diagnostic_id) || `finding-${index + 1}`;
  const kind = text(raw.kind) || text(raw.category) || 'static_site_finding';
  const category = text(raw.category) || kind;
  const path = text(raw.path) || text(raw.source_path) || '';
  const reason = text(raw.reason) || text(raw.message) || text(raw.excerpt) || text(raw.preview) || '';
  const group_key = text(raw.group_key) || text(raw.groupKey) || text(raw.suggested_repair_class) || text(raw.candidate_repo) || category;

  return {
    id,
    kind,
    category,
    group_key,
    path,
    source_path: text(raw.source_path) || path,
    selector: text(raw.selector),
    severity: text(raw.severity) || 'warning',
    reason,
    repair_mode: text(raw.repair_mode),
    artifact_refs: normalizeArtifactRefs([
      ...(normalizeArray(raw.artifact_refs || raw.artifacts).map((ref) => ({ source: 'finding', ...objectRef(ref) }))),
      ...normalizeArray(raw.diagnostic_refs).map((ref) => ({ artifact_id: ref, kind: 'diagnostic' })),
      ...normalizeArray(raw.asset_map_refs).map((ref) => ({ artifact_id: ref, kind: 'asset_map' })),
    ]),
    raw,
    index,
  };
}

function normalizeFanoutGroups(groups, findings, input = {}) {
  if (Array.isArray(groups) && groups.length > 0) {
    return groups.map((group, index) => normalizeFanoutGroup(group, index));
  }
  return groupFanoutItems(findings, {
    group_key: typeof input.group_key === 'function'
      ? input.group_key
      : (finding) => finding.group_key || finding.kind || finding.id,
  });
}

function normalizeFanoutGroup(group, index) {
  const key = text(group.key) || text(group.group_key) || text(group.id) || `group-${index + 1}`;
  const items = normalizeFindings(group.items || group.findings || group.packets || []);
  return {
    key,
    index,
    items,
    artifact_refs: normalizeArtifactRefs(group.artifact_refs || group.artifacts || []),
    raw: group,
  };
}

function createTaskRequest(group, orchestrator, options = {}) {
  if (codeboxCompatibilityRequested(options)) {
    return createWpCodeboxTaskRequest(group, orchestrator, options);
  }
  return createAgentTaskRequest(group, orchestrator, options);
}

function createAgentTaskRequest(group, orchestrator, options = {}) {
  const runtime = {
    ...DEFAULT_AGENT_TASK_PRESET,
    ...(options.agent_task || options.agentTask || {}),
    backend: orchestrator.backend || (options.agent_task || options.agentTask || {}).backend,
  };
  const taskId = taskIdForGroup(group, orchestrator, 'static-site');
  const runtimeTaskInput = {
    task: runtime.runtime_task || runtime.runtimeTask || DEFAULT_AGENT_TASK_PRESET.runtime_task,
    prompt: staticSitePrompt(group),
    artifact_outputs: options.artifact_outputs || options.artifactOutputs || [
      { schema: 'homeboy/static-site-revalidation-outcome/v1', path: '/artifacts/static-site-revalidation-outcome.json' },
    ],
    ...(runtime.runtime_task_input || runtime.runtimeTaskInput || {}),
  };
  return stripUndefined({
    schema: AGENT_TASK_REQUEST_SCHEMA,
    task_id: taskId,
    group_key: group.key,
    parent_plan_id: orchestrator.parent_plan_id || orchestrator.plan_id,
    executor: stripUndefined({
      backend: runtime.backend,
      secret_env: orchestrator.secret_env.length > 0 ? orchestrator.secret_env : undefined,
      config: stripUndefined({
        provider: orchestrator.provider || undefined,
        model: orchestrator.model || undefined,
        provider_plugin_paths: orchestrator.provider_plugin_paths.length > 0 ? orchestrator.provider_plugin_paths : undefined,
        runtime_task: runtimeTaskInput,
        ...(runtime.config || {}),
      }),
    }),
    instructions: options.instructions || staticSiteInstructions(group),
    expected_artifacts: options.expected_artifacts || options.expectedArtifacts || ['typed-artifact-refs', 'runtime-transcript'],
    limits: stripUndefined({
      task_timeout_seconds: runtime.task_timeout_seconds || runtime.taskTimeoutSeconds || options.task_timeout_seconds || options.taskTimeoutSeconds || 900,
      ...(options.limits || {}),
    }),
    inputs: stripUndefined({
      title: `Resolve static-site validation group ${group.key}`,
      preset: orchestrator.preset,
      controller_id: orchestrator.controller_id,
      finding_ids: group.items.map((finding) => finding.id),
      findings: group.items.map(publicFinding),
      artifact_refs: groupArtifactRefs(group),
      ...(options.inputs || {}),
    }),
  });
}

function createWpCodeboxTaskRequest(group, orchestrator, options = {}) {
  const taskId = taskIdForGroup(group, orchestrator, 'static-site-codebox');
  return stripUndefined({
    schema: CODEBOX_COMPATIBILITY_TASK_SCHEMA,
    id: taskId,
    sandbox_session_id: taskId,
    group_key: group.key,
    goal: staticSitePrompt(group),
    expected_artifacts: options.expected_artifacts || options.expectedArtifacts || ['typed-artifact-refs', 'patch', 'review'],
    policy: { kind: orchestrator.preset },
    orchestrator: {
      id: orchestrator.id,
      run_id: orchestrator.run_id,
      plan_id: orchestrator.plan_id,
      controller_id: orchestrator.controller_id,
      source: orchestrator.source,
      group_index: group.index,
    },
    context: {
      preset: orchestrator.preset,
      findings: group.items.map(publicFinding),
      artifact_refs: groupArtifactRefs(group),
      ...(options.context || {}),
    },
    task: {
      title: `Resolve static-site validation group ${group.key}`,
      prompt: staticSitePrompt(group),
    },
    provider: orchestrator.provider || undefined,
    model: orchestrator.model || undefined,
    provider_plugin_paths: orchestrator.provider_plugin_paths.length > 0 ? orchestrator.provider_plugin_paths : undefined,
    secret_env: orchestrator.secret_env.length > 0 ? orchestrator.secret_env : undefined,
  });
}

function staticSiteInstructions(group) {
  return [
    `Resolve static-site import validation group ${group.key}.`,
    'Return typed artifact references for every issue, pull request, patch, revalidation result, or no-op decision produced by the task.',
    'If the group has no actionable findings, return the no_actionable_findings outcome with a concise justification.',
  ].join('\n');
}

function staticSitePrompt(group) {
  if (!group.items.length) {
    return 'No actionable static-site validation findings were supplied. Finish with the no_actionable_findings outcome.';
  }
  return [
    `Process static-site import validation group ${group.key}.`,
    'Findings:',
    JSON.stringify(group.items.map(publicFinding), null, 2),
    'Artifact refs:',
    JSON.stringify(groupArtifactRefs(group), null, 2),
  ].join('\n\n');
}

function publicFinding(finding) {
  return {
    id: finding.id,
    kind: finding.kind,
    category: finding.category,
    severity: finding.severity,
    path: finding.path,
    source_path: finding.source_path,
    selector: finding.selector,
    reason: finding.reason,
    repair_mode: finding.repair_mode,
    artifact_refs: finding.artifact_refs,
  };
}

function groupArtifactRefs(group) {
  return normalizeArtifactRefs([
    ...(group.artifact_refs || []),
    ...group.items.flatMap((finding) => finding.artifact_refs || []),
  ]);
}

async function executeStaticSiteFanout(input = {}) {
  const plan = input.plan || createStaticSiteFanoutPlan(input);
  const executeTaskRequest = staticSiteTaskExecutor(input, plan);

  return executeFanoutReconcileRun({
    ...input,
    plan,
    run_schema: input.run_schema || RUN_SCHEMA,
    base_run: { static_site: plan.static_site },
    runs_output_path: input.runsOutputPath || input.runs_output_path,
    execute_task_request: executeTaskRequest,
    classify_outcome: (record) => record.outcome,
    reconcile: ({ records, outcomes }) => staticSiteReconciliation({ plan, records, outcomes, groups: plan.groups }),
    is_record_successful: (record) => record.status === 'completed' || successfulOutcome(record.outcome),
    task_id: taskRequestId,
    running_entry: runningGroup,
    progress_event: staticSiteProgressEvent,
    task_order: (record) => taskOrder(plan, record),
  });
}

function staticSiteTaskExecutor(input, plan) {
  if (typeof input.execute_task_request === 'function') {
    return input.execute_task_request;
  }
  if ((plan.task_requests || []).length === 0) {
    return async () => ({ status: 'completed', outcome: noActionableFindingsOutcome('No actionable findings were supplied.') });
  }
  throw new Error('static-site fanout execution requires execute_task_request for non-empty plans');
}

function staticSiteReconciliation({ plan, records = [], outcomes = [], groups = [] }) {
  const taskRequests = plan.task_requests || [];
  const no_actionable_findings = taskRequests.length === 0 || outcomes.some((outcome) => outcome?.kind === 'no_actionable_findings');
  const groupRecords = taskRequests.map((taskRequest, index) => {
    const record = records.find((candidate) => taskRequestId(candidate) === taskRequestId(taskRequest));
    const outcome = record?.outcome || outcomes.find((candidate) => candidate?.group_key === taskRequest.group_key) || null;
    return {
      group_key: taskRequest.group_key,
      task_id: taskRequestId(taskRequest),
      status: record?.status || (outcome ? outcome.kind : 'pending'),
      finding_ids: findingIdsForTaskRequest(taskRequest),
      outcome_kind: outcome?.kind || '',
      artifact_refs: normalizeArtifactRefs([
        ...artifactRefsFromTaskRequest(taskRequest),
        ...artifactRefsFromOutcome(outcome),
        ...artifactRefsFromRecord(record),
      ]),
      group_index: index,
    };
  });

  return {
    schema: RECONCILIATION_SCHEMA,
    status: no_actionable_findings && taskRequests.length === 0 ? 'no_actionable_findings' : 'reconciled',
    no_actionable_findings,
    group_count: groups.length,
    task_count: taskRequests.length,
    groups: groupRecords,
    artifact_refs: normalizeArtifactRefs(groupRecords.flatMap((group) => group.artifact_refs)),
  };
}

function noActionableFindingsOutcome(reason) {
  return {
    schema: 'homeboy/static-site-fanout-outcome/v1',
    kind: 'no_actionable_findings',
    reason,
    artifact_refs: [],
  };
}

function successfulOutcome(outcome) {
  return ['artifact_refs', 'fix_artifact', 'fix_pr', 'no_actionable_findings', 'noop_artifact'].includes(outcome?.kind);
}

function artifactRefsFromTaskRequest(taskRequest) {
  return normalizeArtifactRefs(taskRequest.inputs?.artifact_refs || taskRequest.context?.artifact_refs || []);
}

function artifactRefsFromRecord(record) {
  if (!record) {
    return [];
  }
  return normalizeArtifactRefs([
    ...(record.artifact_refs || []),
    ...(record.artifact ? [record.artifact] : []),
    ...(record.result?.artifacts ? [record.result.artifacts] : []),
  ]);
}

function artifactRefsFromOutcome(outcome) {
  if (!outcome) {
    return [];
  }
  return normalizeArtifactRefs([
    ...(outcome.artifact_refs || []),
    ...(outcome.artifacts ? [outcome.artifacts] : []),
    ...(outcome.artifact ? [outcome.artifact] : []),
    ...(outcome.pr_url ? [{ artifact_id: outcome.pr_url, kind: 'pull_request', url: outcome.pr_url }] : []),
    ...(outcome.issue_url ? [{ artifact_id: outcome.issue_url, kind: 'issue', url: outcome.issue_url }] : []),
  ]);
}

function normalizeArtifactRefs(refs) {
  const seen = new Set();
  return normalizeArray(refs).flatMap((ref) => {
    const normalized = normalizeArtifactRef(ref);
    if (!normalized) {
      return [];
    }
    const key = `${normalized.kind}:${normalized.artifact_id || normalized.path || normalized.url}`;
    if (seen.has(key)) {
      return [];
    }
    seen.add(key);
    return [normalized];
  });
}

function normalizeArtifactRef(ref) {
  const value = objectRef(ref);
  const artifact_id = text(value.artifact_id) || text(value.artifactId) || text(value.id) || text(value.url) || text(value.path);
  const path = text(value.path) || text(value.file) || text(value.directory);
  const url = text(value.url) || text(value.pr_url) || text(value.issue_url) || (/^https?:\/\//.test(artifact_id) ? artifact_id : '');
  if (!artifact_id && !path && !url) {
    return null;
  }
  return stripUndefined({
    schema: value.schema || 'homeboy/artifact-ref/v1',
    artifact_id,
    kind: text(value.kind) || text(value.type) || 'artifact',
    path,
    url,
    source: text(value.source),
    role: text(value.role),
  });
}

function taskIdForGroup(group, orchestrator, prefix) {
  return `${prefix}-${safeSlug(orchestrator.run_id)}-${safeSlug(group.key)}`;
}

function taskRequestId(taskRequest) {
  return taskRequest.task_id || taskRequest.id || taskRequest.sandbox_session_id || taskRequest.group_key;
}

function findingIdsForTaskRequest(taskRequest) {
  return normalizeArray(taskRequest.inputs?.finding_ids || taskRequest.context?.findings || [])
    .map((finding) => (typeof finding === 'string' ? finding : finding.id))
    .filter(Boolean);
}

function runningGroup(taskRequest) {
  return {
    task_id: taskRequestId(taskRequest),
    group_key: taskRequest.group_key,
    finding_ids: findingIdsForTaskRequest(taskRequest),
  };
}

function staticSiteProgressEvent(status, taskRequest, plan, record = null) {
  return {
    schema: 'homeboy/static-site-fanout-progress/v1',
    status,
    task_id: taskRequestId(taskRequest),
    group_key: taskRequest.group_key,
    group_index: Number(taskRequest.orchestrator?.group_index || 0) + 1,
    group_count: Number(plan.summary?.group_count || plan.task_requests.length || 0),
    outcome_kind: record?.outcome?.kind || '',
  };
}

function taskOrder(plan, record) {
  return plan.task_requests.findIndex((taskRequest) => taskRequestId(taskRequest) === taskRequestId(record));
}

function requestSchema(input = {}) {
  return codeboxCompatibilityRequested(input) ? CODEBOX_COMPATIBILITY_TASK_SCHEMA : AGENT_TASK_REQUEST_SCHEMA;
}

function codeboxCompatibilityRequested(input = {}) {
  const compatibilityProvider = input.compatibility_provider || input.compatibilityProvider || input.request_provider || input.requestProvider;
  return (input.request_kind || input.requestKind) === 'wp-codebox'
    || compatibilityProvider === CODEBOX_COMPATIBILITY_PROVIDER
    || input.codebox_compatibility === true
    || input.codeboxCompatibility === true
    || input.wp_codebox_compatibility === true
    || input.wpCodeboxCompatibility === true;
}

function safeSlug(value) {
  return String(value || '')
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/-{2,}/g, '-')
    .replace(/^-+|-+$/g, '') || 'static-site';
}

function objectRef(value) {
  if (value && typeof value === 'object') {
    return value;
  }
  return { artifact_id: String(value || '') };
}

function text(value) {
  return typeof value === 'string' ? value.trim() : '';
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function stripUndefined(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
  PLAN_SCHEMA,
  RUN_SCHEMA,
  RECONCILIATION_SCHEMA,
  AGENT_TASK_REQUEST_SCHEMA,
  CODEBOX_COMPATIBILITY_PROVIDER,
  DEFAULT_PRESET,
  COMPATIBILITY_AGENT_TASK_BACKEND,
  createStaticSiteFanoutPlan,
  executeStaticSiteFanout,
  normalizeArtifactRefs,
  normalizeFinding,
  normalizeFindings,
  normalizeFanoutGroups,
  noActionableFindingsOutcome,
  staticSiteReconciliation,
};
