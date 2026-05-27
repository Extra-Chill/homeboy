'use strict';

/* eslint-disable camelcase */

/**
 * External dependencies
 */
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
  ADAPTER_ID,
  loadWpCodeboxArtifactBundle,
  verifyWpCodeboxPayload,
} = require('./wp-codebox-apply-adapter');

const PLAN_SCHEMA = 'homeboy/audit-wp-codebox-fanout/v1';
const TASK_SCHEMA = 'homeboy/wp-codebox-task-request/v1';
const RUN_SCHEMA = 'homeboy/audit-wp-codebox-run/v1';

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

function tryParseJson(value) {
  const trimmed = String(value || '').trim();
  if (!trimmed) {
    return null;
  }
  try {
    return JSON.parse(trimmed);
  } catch {
    return null;
  }
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

function sandboxSessionId(orchestrator, group) {
  return `homeboy-audit-${sha256(JSON.stringify({
    run_id: orchestrator.run_id,
    report_id: orchestrator.report_id,
    group_key: group.key,
    finding_ids: group.findings.map((finding) => finding.id),
  })).slice(0, 16)}`;
}

function taskPrompt(group) {
  const findingList = group.findings
    .map((finding) => `- ${finding.id}: ${finding.kind} in ${finding.file}${finding.line ? `:${finding.line}` : ''}`)
    .join('\n');

  return [
    `Fix the grouped Homeboy audit findings for batch ${group.key}.`,
    'Return a WP Codebox artifact bundle with changed-files, patch, and review metadata.',
    findingList,
  ].join('\n\n');
}

function createTaskRequest(group, orchestrator) {
  const sandbox_session_id = sandboxSessionId(orchestrator, group);
  const request = {
    schema: TASK_SCHEMA,
    sandbox_session_id,
    group_key: group.key,
    orchestrator: {
      id: orchestrator.id,
      run_id: orchestrator.run_id,
      report_id: orchestrator.report_id,
      source: 'homeboy audit',
      issue_url: orchestrator.issue_url || '',
      group_index: group.index,
    },
    audit_findings: group.findings.map((finding) => ({
      id: finding.id,
      fingerprint: finding.fingerprint,
      kind: finding.kind,
      file: finding.file,
      line: finding.line,
      message: finding.message,
      severity: finding.severity,
    })),
    task: {
      title: `Fix Homeboy audit batch ${group.key}`,
      prompt: taskPrompt(group),
    },
  };

  if (orchestrator.provider) {
    request.provider = orchestrator.provider;
  }
  if (orchestrator.model) {
    request.model = orchestrator.model;
  }
  if (orchestrator.provider_plugin_paths.length > 0) {
    request.provider_plugin_paths = orchestrator.provider_plugin_paths;
  }
  if (orchestrator.secret_env.length > 0) {
    request.secret_env = orchestrator.secret_env;
  }

  return request;
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
  const bundle = loadWpCodeboxArtifactBundle(artifactEntry.bundle_path);
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
  const title = artifactEntry.pr_title || `Fix Homeboy audit batch ${taskRequest.group_key}`;
  const issueUrl = options.issue_url || taskRequest.orchestrator.issue_url || '';

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
        `Applies WP Codebox artifact ${verified.artifactId} for Homeboy audit batch ${taskRequest.group_key}.`,
      ].filter(Boolean).join('\n'),
      labels: artifactEntry.labels || ['homeboy-audit', 'wp-codebox'],
    },
  };
}

function createIssueReport(taskRequest, artifactEntry, options) {
  const bundle = artifactEntry.bundle_path ? loadWpCodeboxArtifactBundle(artifactEntry.bundle_path) : null;
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

  const task_requests = groups.map((group) => createTaskRequest(group, orchestrator));
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

function executeWpCodeboxTaskRequest(taskRequest, options = {}) {
  const command = options.wp_codebox_command || 'wp-codebox';
  const args = options.wp_codebox_args || [];
  const requestJson = `${JSON.stringify(taskRequest, null, 2)}\n`;
  const startedAt = new Date().toISOString();
  const result = spawnSync(command, args, {
    cwd: options.cwd || process.cwd(),
    encoding: 'utf8',
    env: {
      ...process.env,
      ...(options.env || {}),
      HOMEBOY_WP_CODEBOX_TASK_REQUEST: requestJson,
      HOMEBOY_WP_CODEBOX_SANDBOX_SESSION_ID: taskRequest.sandbox_session_id,
      HOMEBOY_WP_CODEBOX_GROUP_KEY: taskRequest.group_key,
    },
    input: requestJson,
    maxBuffer: options.max_buffer || 1024 * 1024 * 10,
  });
  const finishedAt = new Date().toISOString();
  const stdout = result.stdout || '';
  const stderr = result.stderr || '';
  const parsed = tryParseJson(stdout);
  const artifact = parsed && typeof parsed === 'object' && parsed.artifacts ? parsed.artifacts : null;
  const success = result.status === 0 && result.error === undefined;

  return {
    schema: RUN_SCHEMA,
    sandbox_session_id: taskRequest.sandbox_session_id,
    group_key: taskRequest.group_key,
    finding_ids: taskRequest.audit_findings.map((finding) => finding.id),
    command: {
      bin: command,
      args,
      exit_code: result.status,
      signal: result.signal || null,
      error: result.error ? result.error.message : '',
    },
    status: success ? 'completed' : 'failed',
    started_at: startedAt,
    finished_at: finishedAt,
    stdout,
    stderr,
    result: parsed,
    artifact: artifact ? {
      id: artifact.id || '',
      directory: artifact.directory || artifact.path || '',
      preview_url: artifact.preview?.url || artifact.preview_url || '',
    } : null,
  };
}

function executeAuditWpCodeboxFanout(input) {
  const plan = input.plan || createAuditWpCodeboxFanoutPlan(input);

  const records = [];
  const baseRun = {
    schema: 'homeboy/audit-wp-codebox-execution/v1',
    plan_schema: plan.schema,
    orchestrator: plan.orchestrator,
    audit: plan.audit,
  };
  const writeRun = (run) => {
    if (input.runsOutputPath) {
      writeJson(input.runsOutputPath, run);
    }
  };

  writeRun({
    ...baseRun,
    records,
    status: 'incomplete',
    current_group: null,
  });

  for (const taskRequest of plan.task_requests) {
    writeRun({
      ...baseRun,
      records,
      status: 'incomplete',
      current_group: {
        sandbox_session_id: taskRequest.sandbox_session_id,
        group_key: taskRequest.group_key,
        finding_ids: taskRequest.audit_findings.map((finding) => finding.id),
      },
    });

    records.push(executeWpCodeboxTaskRequest(taskRequest, input));

    writeRun({
      ...baseRun,
      records,
      status: 'incomplete',
      current_group: null,
    });
  }

  const run = {
    ...baseRun,
    records,
    status: records.every((record) => record.status === 'completed') ? 'completed' : 'failed',
  };

  writeRun(run);

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
    runsOutputPath: options.runsOutputPath,
  });
}

module.exports = {
  PLAN_SCHEMA,
  RUN_SCHEMA,
  TASK_SCHEMA,
  auditFindings,
  createAuditWpCodeboxFanoutPlan,
  createAuditWpCodeboxFanoutPlanFromFiles,
  executeAuditWpCodeboxFanout,
  executeAuditWpCodeboxFanoutFromFiles,
  executeWpCodeboxTaskRequest,
  createIssueReport,
  groupFindings,
  safeBranchSlug,
  sandboxSessionId,
};
