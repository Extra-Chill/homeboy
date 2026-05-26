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
  loadWpCodeboxArtifactBundle,
  verifyWpCodeboxPayload,
} = require('./wp-codebox-apply-adapter');

const PLAN_SCHEMA = 'homeboy/audit-wp-codebox-fanout/v1';
const TASK_SCHEMA = 'homeboy/wp-codebox-task-request/v1';

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
  return {
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
}

function safeBranchSlug(value) {
  const slug = String(value || '')
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9._/-]+/g, '-')
    .replace(/-{2,}/g, '-')
    .replace(/\/{2,}/g, '/')
    .replace(/[-/.]+$/g, '')
    .replace(/^[-/.]+/g, '');

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

function createAuditWpCodeboxFanoutPlan(input) {
  const report = input.report || readJson(input.auditReportPath);
  const orchestrator = {
    id: input.orchestrator_id || 'homeboy-extensions/audit-wp-codebox-fanout',
    run_id: input.run_id || report.run_id || report.id || 'fixture-run',
    report_id: input.report_id || report.id || report.run_id || 'fixture-report',
    issue_url: input.issue_url || '',
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
    base: options.base,
    branch_prefix: options.branchPrefix,
    reviewed_at: options.reviewedAt,
  });
  if (options.outputPath) {
    writeJson(options.outputPath, plan);
  }
  return plan;
}

module.exports = {
  PLAN_SCHEMA,
  TASK_SCHEMA,
  auditFindings,
  createAuditWpCodeboxFanoutPlan,
  createAuditWpCodeboxFanoutPlanFromFiles,
  groupFindings,
  safeBranchSlug,
  sandboxSessionId,
};
