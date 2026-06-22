'use strict';

/* eslint-disable camelcase */

/**
 * External dependencies
 */
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
  ADAPTER_ID,
  verifyWpCodeboxPayload,
  loadArtifactBundle,
  runAgentTask,
  wpCodeboxApplyRequestFromBundle,
  wpCodeboxChangeArtifactFromBundle,
} = require('./codebox-provider-adapter');
const {
  executeFanoutReconcileRun,
} = require('../../runtime-agent-ci/lib/fanout-reconcile-runner');
const {
  DEFAULT_TASK_TIMEOUT_SECONDS,
  RUN_SCHEMA,
  TASK_SCHEMA,
  createWpCodeboxTaskRequest,
  executeWpCodeboxTaskRequest,
  pullRequestUrls,
  sandboxSessionId,
  taskOutcome,
  taskOutcomeSucceeded,
} = require('./codebox-provider-adapter');

const PLAN_SCHEMA = 'homeboy/audit-wp-codebox-fanout/v1';
const IMPLEMENTATION_SCOPE = Object.freeze({
  id: 'wordpress.audit-wp-codebox-fanout',
	quarantine: 'wp-codebox-compatibility-entrypoint',
	generic_surface: false,
  public_entrypoints: [
    'wordpress/lib/audit-wp-codebox-fanout.js',
    'wordpress/scripts/agent/homeboy-audit-wp-codebox-fanout.cjs',
  ],
  runtime_adapter: 'wordpress/lib/audit-fanout-runtime-adapter.js',
  rationale: 'Audit fanout keeps the legacy WP Codebox entrypoint while runtime dispatch is delegated through the generic audit fanout runtime adapter seam.',
});
const DEFAULT_CONCURRENCY = 3;

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex');
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function progressEvent(status, taskRequest, plan, record = null) {
  const groupIndex = Number(taskRequest.orchestrator?.group_index || 0) + 1;
  const groupCount = Number(plan.audit?.group_count || plan.task_requests.length || 0);
  const startedAt = record?.started_at || new Date().toISOString();
  const finishedAt = record?.finished_at || '';
  const elapsedMs = finishedAt ? Math.max(0, Date.parse(finishedAt) - Date.parse(startedAt)) : null;

  return {
    schema: 'homeboy/audit-wp-codebox-progress/v1',
    status,
    group_key: taskRequest.group_key,
    finding_id: taskRequest.finding_id || taskRequest.audit_findings?.[0]?.id || '',
    group_index: groupIndex,
    group_count: groupCount,
    sandbox_session_id: taskRequest.sandbox_session_id,
    started_at: startedAt,
    finished_at: finishedAt,
    elapsed_ms: elapsedMs,
    artifact_directory: record?.artifact?.directory || '',
    outcome_kind: record?.outcome?.kind || '',
    retryable: record?.outcome?.retryable ?? null,
    failure: record?.outcome?.failure || '',
  };
}

function auditFindings(report) {
  const candidates = [
    report?.data?.findings,
    report?.findings,
    report?.data?.top_findings,
    report?.top_findings,
  ];
  const findings = candidates.find((value) => Array.isArray(value)) || [];
  return findings.map((finding, index) => normalizeFinding(finding, index));
}

function normalizeFinding(finding, index) {
  const kind = finding.kind || finding.code || finding.rule || 'unknown';
  const file = finding.file || finding.path || finding.location?.file || '';
  const line = Number(finding.line || finding.location?.line || 0);
  const fingerprint = finding.fingerprint || sha256(`${kind}\n${file}\n${line}\n${finding.message || ''}`);

  return {
    id: finding.id || fingerprint,
    fingerprint,
    kind,
    file,
    line,
    message: finding.message || finding.description || '',
    severity: finding.severity || finding.level || 'warning',
    fix_batch_key: finding.fix_batch_key || finding.fixBatchKey || kind,
    raw: finding,
    index,
  };
}

function groupFindings(findings) {
  const groupsByKey = new Map();
  for (const finding of findings) {
    const key = finding.fix_batch_key || finding.kind;
    if (!groupsByKey.has(key)) {
      groupsByKey.set(key, []);
    }
    groupsByKey.get(key).push(finding);
  }

  return Array.from(groupsByKey.entries()).map(([key, groupedFindings], index) => ({
    key,
    index,
    findings: groupedFindings,
  }));
}

function safeBranchSlug(value) {
  const slug = String(value || '')
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/-{2,}/g, '-')
    .replace(/^-+|-+$/g, '');

  return slug || 'audit-batch';
}

function explicitApprovedFiles(artifactEntry) {
  if (!Array.isArray(artifactEntry.approved_files) || artifactEntry.approved_files.length === 0) {
    throw new Error('apply-back artifact entries require non-empty explicit approved_files');
  }

  return artifactEntry.approved_files;
}

function createApplyBackMetadata(taskRequest, artifactEntry, options) {
  const bundle = loadArtifactBundle(artifactEntry.bundle_path);
  const approvedFiles = explicitApprovedFiles(artifactEntry);
  const verified = verifyWpCodeboxPayload({
    artifact_id: bundle.id,
    artifact: bundle,
    approved_files: approvedFiles,
    patch: bundle.patch,
    patch_sha256: artifactEntry.patch_sha256,
    artifact_content_digest: artifactEntry.artifact_content_digest,
  });
  const branch = artifactEntry.branch || `${options.branch_prefix || 'fix/homeboy-audit'}/${safeBranchSlug(taskRequest.group_key)}`;
  const title = artifactEntry.pr_title || `Fix Homeboy audit finding ${taskRequest.group_key}`;
  const issueUrl = options.issue_url || taskRequest.orchestrator.issue_url || '';
  const changeArtifact = wpCodeboxChangeArtifactFromBundle(bundle, {
    runId: taskRequest.orchestrator?.run_id,
    stepId: taskRequest.sandbox_session_id,
    title,
    summary: `WP Codebox patch for Homeboy audit finding ${taskRequest.group_key}.`,
  });
  const applyRequest = wpCodeboxApplyRequestFromBundle({
    id: `apply-request-${verified.artifactId}`,
    bundle,
    approvedFiles,
    branch,
    commitMessage: artifactEntry.commit_message || title,
    patchStrip: artifactEntry.patch_strip,
    push: false,
    openPullRequest: false,
    prBase: artifactEntry.base || options.base || 'main',
    runId: taskRequest.orchestrator?.run_id,
    stepId: taskRequest.sandbox_session_id,
    title,
    summary: `WP Codebox patch for Homeboy audit finding ${taskRequest.group_key}.`,
  });

  return {
    schema: 'homeboy/wp-codebox-apply-back/v1',
    adapter_id: ADAPTER_ID,
    sandbox_session_id: taskRequest.sandbox_session_id,
    group_key: taskRequest.group_key,
    finding_ids: taskRequest.audit_findings.map((finding) => finding.id),
    artifact: {
      id: verified.artifactId,
      bundle_path: bundle.directory,
      content_digest: verified.contentDigest,
      patch_sha256: verified.patchSha256,
      review: bundle.review,
      changed_files: bundle.changed_files,
    },
    change_artifact: changeArtifact,
    apply_request: applyRequest,
    review: {
      approved: artifactEntry.approved !== false,
      approved_files: approvedFiles,
      reviewer: artifactEntry.reviewer || 'fixture-reviewer',
      reviewed_at: artifactEntry.reviewed_at || options.reviewed_at || '',
    },
    adapter_payload: {
      bundlePath: bundle.directory,
      approvedFiles,
      branch,
      commitMessage: artifactEntry.commit_message || title,
      patchStrip: artifactEntry.patch_strip,
      push: false,
      openPullRequest: false,
    },
    pull_request: {
      title,
      base: artifactEntry.base || options.base || 'main',
      head: branch,
      body: [
        issueUrl ? `Closes ${issueUrl}` : '',
        '',
        `Applies WP Codebox artifact ${verified.artifactId} for Homeboy audit finding ${taskRequest.group_key}.`,
      ].filter(Boolean).join('\n'),
      labels: artifactEntry.labels || ['homeboy-audit', 'wp-codebox'],
    },
  };
}

function createIssueReport(taskRequest, artifactEntry, options) {
  const bundle = artifactEntry.bundle_path ? loadArtifactBundle(artifactEntry.bundle_path) : null;
  const issueUrl = options.issue_url || taskRequest.orchestrator.issue_url || '';
  const disposition = artifactEntry.disposition || (artifactEntry.false_positive ? 'false_positive' : 'rejected_artifact');
  const reason = artifactEntry.reason || artifactEntry.false_positive_reason || artifactEntry.rejection_reason || '';
  const title = artifactEntry.issue_title || (
    disposition === 'false_positive'
      ? `Review Homeboy audit false positive for ${taskRequest.group_key}`
      : `Review rejected WP Codebox artifact for ${taskRequest.group_key}`
  );
  const body = artifactEntry.issue_body || [
    issueUrl ? `Source tracker: ${issueUrl}` : '',
    '',
    `Disposition: ${disposition}`,
    reason ? `Reason: ${reason}` : '',
    '',
    'Findings:',
    ...taskRequest.audit_findings.map((finding) => `- ${finding.id}: ${finding.kind} in ${finding.file}${finding.line ? `:${finding.line}` : ''}`),
    bundle ? '' : '',
    bundle ? `WP Codebox artifact: ${bundle.id}` : '',
  ].filter(Boolean).join('\n');

  return {
    schema: 'homeboy/audit-wp-codebox-issue-report/v1',
    sandbox_session_id: taskRequest.sandbox_session_id,
    group_key: taskRequest.group_key,
    disposition,
    reason,
    finding_ids: taskRequest.audit_findings.map((finding) => finding.id),
    artifact: bundle ? {
      id: bundle.id,
      bundle_path: bundle.directory,
      review: bundle.review,
      changed_files: bundle.changed_files,
    } : null,
    issue: {
      title,
      body,
      labels: artifactEntry.labels || ['homeboy-audit', 'wp-codebox', disposition],
    },
  };
}

function createAuditWpCodeboxFanoutPlan(input) {
  const report = input.report || readJson(input.auditReportPath);
  const orchestrator = {
    id: input.orchestrator_id || 'homeboy-extensions/audit-wp-codebox-fanout',
    run_id: input.run_id || report.run_id || report.id || 'fixture-run',
    report_id: input.report_id || report.id || report.run_id || 'fixture-report',
    issue_url: input.issue_url || '',
    provider: input.provider || '',
    model: input.model || '',
    provider_plugin_paths: Array.isArray(input.provider_plugin_paths) ? input.provider_plugin_paths : [],
    secret_env: Array.isArray(input.secret_env) ? input.secret_env : [],
  };
  const groups = groupFindings(auditFindings(report));
  const artifactMap = input.artifact_map || {};
  const options = {
    base: input.base || 'main',
    branch_prefix: input.branch_prefix || 'fix/homeboy-audit',
    issue_url: input.issue_url || '',
    reviewed_at: input.reviewed_at || '',
  };

  const task_requests = groups.map((group) => createWpCodeboxTaskRequest(group, orchestrator));
  const issue_reports = task_requests
    .map((taskRequest) => {
      const artifactEntry = artifactMap[taskRequest.sandbox_session_id] || artifactMap[taskRequest.group_key];
      return artifactEntry?.approved === false ? createIssueReport(taskRequest, artifactEntry, options) : null;
    })
    .filter(Boolean);
  const apply_back = task_requests
    .map((taskRequest) => {
      const artifactEntry = artifactMap[taskRequest.sandbox_session_id] || artifactMap[taskRequest.group_key];
      if (artifactEntry?.approved === false) {
        return null;
      }
      return artifactEntry ? createApplyBackMetadata(taskRequest, artifactEntry, options) : null;
    })
    .filter(Boolean);

  return {
    schema: PLAN_SCHEMA,
    orchestrator,
    audit: {
      report_id: orchestrator.report_id,
      finding_count: task_requests.reduce((count, request) => count + request.audit_findings.length, 0),
      group_count: task_requests.length,
      task_count: task_requests.length,
    },
    task_requests,
    apply_back,
    issue_reports,
  };
}

function createAuditWpCodeboxFanoutPlanFromFiles(options) {
  const artifact_map = options.artifactMapPath ? readJson(options.artifactMapPath) : {};
  const plan = createAuditWpCodeboxFanoutPlan({
    auditReportPath: options.auditReportPath,
    artifact_map,
    orchestrator_id: options.orchestratorId,
    run_id: options.runId,
    report_id: options.reportId,
    issue_url: options.issueUrl,
    provider: options.provider,
    model: options.model,
    provider_plugin_paths: options.providerPluginPaths || [],
    secret_env: options.secretEnv || [],
    base: options.base,
    branch_prefix: options.branchPrefix,
    reviewed_at: options.reviewedAt,
  });
  if (options.outputPath) {
    writeJson(options.outputPath, plan);
  }
  return plan;
}

async function executeAuditWpCodeboxFanout(input) {
  const plan = input.plan || createAuditWpCodeboxFanoutPlan(input);
  const run = await executeFanoutReconcileRun({
    ...input,
    plan,
    run_schema: 'homeboy/audit-wp-codebox-execution/v1',
    base_run: { audit: plan.audit },
    include_summary: false,
    include_reconciliation: false,
    runs_output_path: input.runsOutputPath,
    execute_task_request: (taskRequest) => runAgentTask(taskRequest, input),
    classify_outcome: (record) => record.outcome,
    reconcile: () => ({
      apply_back: plan.apply_back || [],
      issue_reports: plan.issue_reports || [],
    }),
    is_record_successful: (record) => record.status === 'completed',
    task_id: (taskRequest) => taskRequest.sandbox_session_id,
    running_entry: runningGroup,
    progress_event: progressEvent,
    task_order: (record) => taskOrder(plan, record),
  });

  return run;
}

function executeAuditWpCodeboxFanoutFromFiles(options) {
  const plan = createAuditWpCodeboxFanoutPlanFromFiles(options);
  return executeAuditWpCodeboxFanout({
    plan,
    wp_codebox_command: options.wpCodeboxCommand,
    wp_codebox_args: options.wpCodeboxArgs || [],
    cwd: options.cwd,
    env: options.env,
    concurrency: options.concurrency,
    task_timeout_seconds: options.taskTimeoutSeconds,
    runsOutputPath: options.runsOutputPath,
    on_progress: options.onProgress,
  });
}

function runningGroup(taskRequest) {
  return {
    sandbox_session_id: taskRequest.sandbox_session_id,
    group_key: taskRequest.group_key,
    finding_id: taskRequest.finding_id || taskRequest.audit_findings[0]?.id || '',
    finding_ids: taskRequest.audit_findings.map((finding) => finding.id),
  };
}

function taskOrder(plan, record) {
  return plan.task_requests.findIndex((taskRequest) => taskRequest.sandbox_session_id === record.sandbox_session_id);
}

module.exports = {
  IMPLEMENTATION_SCOPE,
  PLAN_SCHEMA,
  RUN_SCHEMA,
  TASK_SCHEMA,
  DEFAULT_CONCURRENCY,
  DEFAULT_TASK_TIMEOUT_SECONDS,
  auditFindings,
  createAuditWpCodeboxFanoutPlan,
  createAuditWpCodeboxFanoutPlanFromFiles,
  executeAuditWpCodeboxFanout,
  executeAuditWpCodeboxFanoutFromFiles,
  executeWpCodeboxTaskRequest,
  createIssueReport,
  groupFindings,
  pullRequestUrls,
  safeBranchSlug,
  sandboxSessionId,
  taskOutcome,
  taskOutcomeSucceeded,
};
