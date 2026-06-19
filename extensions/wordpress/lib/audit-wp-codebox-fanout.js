'use strict';

/* eslint-disable camelcase */

/**
 * External dependencies
 */
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
  ADAPTER_ID,
  loadWpCodeboxArtifactBundle,
  verifyWpCodeboxPayload,
  wpCodeboxApplyRequestFromBundle,
  wpCodeboxChangeArtifactFromBundle,
} = require('./wp-codebox-apply-adapter');
const {
  executeFanoutReconcileRun,
} = require('./fanout-reconcile-runner');
const { loadWpCodeboxCoreFunction } = require('./wp-codebox-core-loader');

const PLAN_SCHEMA = 'homeboy/audit-wp-codebox-fanout/v1';
const TASK_SCHEMA = 'wp-codebox/task-input/v1';
const RUN_SCHEMA = 'homeboy/audit-wp-codebox-run/v1';
const IMPLEMENTATION_SCOPE = Object.freeze({
  id: 'wordpress.audit-wp-codebox-fanout',
  quarantine: 'wp-codebox-implementation',
  generic_surface: false,
  public_entrypoints: [
    'wordpress/lib/audit-wp-codebox-fanout.js',
    'wordpress/scripts/agent/homeboy-audit-wp-codebox-fanout.cjs',
  ],
  rationale: 'Audit fanout plans and executes wp-codebox/task-input/v1 requests; callers that need executor-neutral fanout should use generic-fanout-reconcile-workflow instead.',
});
const DEFAULT_CONCURRENCY = 3;
const DEFAULT_TASK_TIMEOUT_SECONDS = 45 * 60;
const WP_CODEBOX_STRUCTURED_OUTCOME_KINDS = new Set([
  'fix_artifact',
  'false_positive_artifact',
  'fix_pr',
  'false_positive_pr',
  'provider_error',
  'agent_no_pr_outcome',
  'noop_artifact',
  'unable_to_remediate',
  'max_turns_exceeded',
]);
const SECRET_KEY_PATTERN = /(secret|token|password|passwd|authorization|cookie|nonce|api[_-]?key|access[_-]?key|private[_-]?key|bearer)/i;

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

function redact(value, key = '') {
  if (SECRET_KEY_PATTERN.test(key)) {
    return '[redacted]';
  }
  if (Array.isArray(value)) {
    return value.map((entry) => redact(entry));
  }
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([entryKey, entryValue]) => [entryKey, redact(entryValue, entryKey)]));
  }
  if (typeof value === 'string') {
    return value.replace(/(bearer|token|api[_-]?key|password|cookie|authorization|private[_-]?key)(\s*[:=]\s*)[^\s,;]+/gi, '$1$2[redacted]');
  }
  return value;
}

function wpCodeboxRuntimeReferenceManifestPath(parsed, artifact) {
  const artifacts = parsed && typeof parsed === 'object' && parsed.artifacts && typeof parsed.artifacts === 'object'
    ? parsed.artifacts
    : {};
  const candidates = [
    artifacts.runtimeReferenceManifestPath,
    artifacts.runtimeReferencesManifestPath,
    artifacts.runtime_reference_manifest_path,
    artifacts.referenceManifestPath,
    artifacts.runtimeReferencePath,
    artifacts.runtimeReferencesPath,
    artifacts.runtime?.referenceManifestPath,
    artifact?.runtimeReferenceManifestPath,
    artifact?.runtime_reference_manifest_path,
  ];
  return candidates.find((candidate) => typeof candidate === 'string' && candidate.trim()) || '';
}

function wpCodeboxRuntimeReferenceManifest(parsed, artifact) {
  const manifestPath = wpCodeboxRuntimeReferenceManifestPath(parsed, artifact);
  if (!manifestPath) {
    return null;
  }

  const manifest = { path: manifestPath, available: false };
  if (fs.existsSync(manifestPath) && fs.statSync(manifestPath).isFile()) {
    manifest.available = true;
    try {
      manifest.payload = redact(readJson(manifestPath));
    } catch (error) {
      manifest.error = error && error.message ? error.message : String(error);
    }
  }
  return manifest;
}

function fanoutRecordMetrics(startedAt, finishedAt, parsed, artifact) {
  const runnerMetrics = parsed && typeof parsed === 'object' && parsed.metrics && typeof parsed.metrics === 'object'
    ? parsed.metrics
    : {};
  const artifactDirectory = artifact?.directory || artifact?.path || '';

  return {
    duration_ms: Math.max(0, Date.parse(finishedAt) - Date.parse(startedAt)),
    peak_rss_bytes: numberOrNull(runnerMetrics.peak_rss_bytes),
    sample_count: numberOrDefault(runnerMetrics.sample_count, 0),
    child_process_count_peak: numberOrNull(runnerMetrics.child_process_count_peak),
    artifact_bytes: artifactDirectory ? directorySizeBytes(artifactDirectory) : null,
    ...(runnerMetrics.cpu_user_ms === undefined ? {} : { cpu_user_ms: numberOrNull(runnerMetrics.cpu_user_ms) }),
    ...(runnerMetrics.cpu_system_ms === undefined ? {} : { cpu_system_ms: numberOrNull(runnerMetrics.cpu_system_ms) }),
    ...(runnerMetrics.source ? { source: runnerMetrics.source } : {}),
  };
}

function artifactsRootFromArgs(args = []) {
  const index = args.indexOf('--artifacts');
  if (index >= 0 && args[index + 1]) {
    return args[index + 1];
  }
  return '';
}

function discoverTaskArtifacts(args, taskRequest, startedAt, finishedAt) {
  const root = artifactsRootFromArgs(args);
  if (!root || !fs.existsSync(root)) {
    return [];
  }

  const startedMs = Date.parse(startedAt);
  const finishedMs = Date.parse(finishedAt);
  const earliestMs = Number.isFinite(startedMs) ? startedMs - 1000 : 0;
  const latestMs = Number.isFinite(finishedMs) ? finishedMs + 1000 : Date.now() + 1000;

  const artifacts = fs.readdirSync(root)
    .map((entry) => artifactEvidence(path.join(root, entry), earliestMs, latestMs))
    .filter(Boolean)
    .sort((left, right) => left.directory.localeCompare(right.directory));
  const sessionArtifacts = artifacts.filter((artifact) => artifact.directory.includes(taskRequest.sandbox_session_id));
  return sessionArtifacts.length > 0 ? sessionArtifacts : artifacts;
}

async function discoverTaskArtifactsAsync(args, taskRequest, startedAt, finishedAt, options = {}) {
  const discoverPartialRunArtifacts = await loadWpCodeboxCoreFunction('discoverPartialRunArtifacts', {
    wpCodeboxCoreModule: options.wpCodeboxCoreModule || options.env?.WP_CODEBOX_CORE_MODULE,
  });
  const root = artifactsRootFromArgs(args);
  if (!discoverPartialRunArtifacts || !root) {
    return discoverTaskArtifacts(args, taskRequest, startedAt, finishedAt);
  }

  try {
    const discovery = await discoverPartialRunArtifacts({
      artifactsRoot: root,
      sessionId: taskRequest.sandbox_session_id,
      startedAt,
      finishedAt,
    });
    return corePartialArtifactsToLegacy(discovery);
  } catch {
    return discoverTaskArtifacts(args, taskRequest, startedAt, finishedAt);
  }
}

function corePartialArtifactsToLegacy(discovery) {
  return (Array.isArray(discovery?.artifacts) ? discovery.artifacts : []).map((artifact) => ({
    directory: artifact.directory,
    bytes: artifact.bytes ?? null,
    mtime: artifact.mtime,
    has_manifest: Boolean(artifact.hasManifest),
    has_changed_files: Boolean(artifact.hasChangedFiles),
    changed_files_path: artifact.changedFiles?.available ? artifact.changedFiles.path : '',
    runtime_reference_manifest: artifact.runtimeReferenceManifest?.available ? {
      path: artifact.runtimeReferenceManifest.path,
      available: true,
      ...(artifact.runtimeReferenceManifest.payload === undefined ? {} : { payload: artifact.runtimeReferenceManifest.payload }),
      ...(artifact.runtimeReferenceManifest.error ? { error: artifact.runtimeReferenceManifest.error } : {}),
    } : null,
  }));
}

function artifactEvidence(directory, earliestMs, latestMs) {
  try {
    const stat = fs.statSync(directory);
    if (!stat.isDirectory() || stat.mtimeMs < earliestMs || stat.mtimeMs > latestMs) {
      return null;
    }
    const changedFilesPath = path.join(directory, 'files', 'changed-files.json');
    const manifestPath = path.join(directory, 'manifest.json');
    const runtimeManifestPath = path.join(directory, 'files', 'runtime-reference-manifest.json');
    return {
      directory,
      bytes: directorySizeBytes(directory),
      mtime: stat.mtime.toISOString(),
      has_manifest: fs.existsSync(manifestPath),
      has_changed_files: fs.existsSync(changedFilesPath),
      changed_files_path: fs.existsSync(changedFilesPath) ? changedFilesPath : '',
      runtime_reference_manifest: runtimeManifestPath && fs.existsSync(runtimeManifestPath)
        ? wpCodeboxRuntimeReferenceManifest({ artifacts: { runtimeReferenceManifestPath: runtimeManifestPath } }, {})
        : null,
    };
  } catch {
    return null;
  }
}

function failureMessageFromCommand(command) {
  if (command.error) {
    return command.error;
  }
  if (command.timed_out) {
    return `WP Codebox task timed out after ${command.timeout_seconds} seconds`;
  }
  if (command.signal) {
    return `WP Codebox task exited from signal ${command.signal}`;
  }
  if (command.exit_code !== 0 && command.exit_code !== null && command.exit_code !== undefined) {
    return `WP Codebox task exited with code ${command.exit_code}`;
  }
  return 'WP Codebox task failed without a structured outcome';
}

function enrichFailureOutcome(outcome, taskRequest, command, startedAt, finishedAt, partialArtifacts = []) {
  if (taskOutcomeSucceeded(outcome)) {
    return outcome;
  }
  const elapsedMs = Math.max(0, Date.parse(finishedAt) - Date.parse(startedAt));
  const failureMetadata = {
    group_key: taskRequest.group_key,
    sandbox_session_id: taskRequest.sandbox_session_id,
    finding_ids: taskRequest.audit_findings.map((finding) => finding.id),
    exit_code: command.exit_code ?? null,
    signal: command.signal || null,
    elapsed_ms: Number.isFinite(elapsedMs) ? elapsedMs : null,
    timed_out: Boolean(command.timed_out),
    timeout_seconds: command.timeout_seconds ?? null,
    killed_process_group: Boolean(command.killed_process_group),
    force_killed_process_group: Boolean(command.force_killed_process_group),
    partial_artifact_count: partialArtifacts.length,
  };
  return {
    ...outcome,
    failure: outcome.failure || failureMessageFromCommand(command),
    failure_metadata: failureMetadata,
    partial_artifacts: partialArtifacts,
  };
}

function numberOrNull(value) {
  return Number.isFinite(Number(value)) ? Number(value) : null;
}

function numberOrDefault(value, fallback) {
  return Number.isFinite(Number(value)) ? Number(value) : fallback;
}

function directorySizeBytes(directory) {
  try {
    const stat = fs.lstatSync(directory);
    if (!stat.isDirectory()) {
      return stat.size;
    }
    return fs.readdirSync(directory).reduce((total, entry) => total + directorySizeBytes(path.join(directory, entry)), 0);
  } catch {
    return null;
  }
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

function sandboxSessionId(orchestrator, group) {
  return `homeboy-audit-${sha256(JSON.stringify({
    run_id: orchestrator.run_id,
    report_id: orchestrator.report_id,
    finding_id: group.findings[0]?.id || group.key,
    finding_ids: group.findings.map((finding) => finding.id),
  })).slice(0, 16)}`;
}

function taskPrompt(group) {
  const findings = group.findings || [];
  const findingList = findings
    .map((finding) => {
      const location = `${finding.file || ''}${finding.line ? `:${finding.line}` : ''}`;
      return `- ${finding.id}: ${finding.kind}${location ? ` in ${location}` : ''}${finding.message ? ` — ${finding.message}` : ''}`;
    })
    .join('\n');

  return [
    `Fix the Homeboy audit remediation group ${group.key}.`,
    '',
    'Expected outcome:',
    '- Produce a reviewed WP Codebox artifact that fixes every finding in this remediation group, or',
    '- If the group is a false positive, produce a reviewed artifact that fixes the audit detector/config/test path that produced it.',
    '',
    'The parent orchestrator applies accepted artifacts and opens pull requests outside the sandbox. Return machine-readable outcome metadata for every terminal result: fix_artifact, false_positive_artifact, noop_artifact, unable_to_remediate, provider_error, max_turns_exceeded, or explicit_failure. The parent run reconciles this outcome back to each finding ID.',
    '',
    'Finding evidence:',
    findingList,
  ].join('\n\n');
}

function createTaskRequest(group, orchestrator) {
  const sandbox_session_id = sandboxSessionId(orchestrator, group);
  const request = {
    schema: TASK_SCHEMA,
    goal: taskPrompt(group),
    expected_artifacts: ['patch', 'review'],
    policy: { kind: 'audit-remediation' },
    context: {
      group_key: group.key,
      audit_findings: group.findings.map((finding) => ({
        id: finding.id,
        fingerprint: finding.fingerprint,
        kind: finding.kind,
        file: finding.file,
        line: finding.line,
        message: finding.message,
        severity: finding.severity,
      })),
    },
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
      title: `Fix Homeboy audit remediation group ${group.key}`,
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
  const metrics = fanoutRecordMetrics(startedAt, finishedAt, parsed, artifact);
  const taskFailure = parsed ? wpCodeboxTaskFailure(parsed) : null;
  const commandSuccess = result.status === 0 && result.error === undefined && null === taskFailure;
  const commandInfo = {
    bin: command,
    args,
    exit_code: result.status,
    signal: result.signal || null,
    error: result.error ? result.error.message : (taskFailure || ''),
  };
  const partialArtifacts = commandSuccess ? [] : discoverTaskArtifacts(args, taskRequest, startedAt, finishedAt);
  const outcome = enrichFailureOutcome(
    taskOutcome(taskRequest, parsed, artifact, commandSuccess, taskFailure || (result.error ? result.error.message : '')),
    taskRequest,
    commandInfo,
    startedAt,
    finishedAt,
    partialArtifacts
  );
  const success = taskOutcomeSucceeded(outcome);

  return {
    schema: RUN_SCHEMA,
    sandbox_session_id: taskRequest.sandbox_session_id,
    group_key: taskRequest.group_key,
    finding_id: taskRequest.finding_id || taskRequest.audit_findings[0]?.id || '',
    finding_ids: taskRequest.audit_findings.map((finding) => finding.id),
    command: commandInfo,
    status: success ? 'completed' : 'failed',
    started_at: startedAt,
    finished_at: finishedAt,
    stdout,
    stderr,
    result: parsed,
    outcome,
    artifact: artifact ? {
      id: artifact.id || '',
      directory: artifact.directory || artifact.path || '',
      preview_url: artifact.preview?.url || artifact.preview_url || '',
      runtime_reference_manifest: wpCodeboxRuntimeReferenceManifest(parsed, artifact),
    } : null,
    partial_artifacts: partialArtifacts,
    metrics,
  };
}

function executeWpCodeboxTaskRequestAsync(taskRequest, options = {}) {
  const command = options.wp_codebox_command || 'wp-codebox';
  const args = options.wp_codebox_args || [];
  const requestJson = `${JSON.stringify(taskRequest, null, 2)}\n`;
  const startedAt = new Date().toISOString();
  const taskTimeoutSeconds = normalizeTaskTimeoutSeconds(options.task_timeout_seconds);

  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd: options.cwd || process.cwd(),
      detached: true,
      env: {
        ...process.env,
        ...(options.env || {}),
        HOMEBOY_WP_CODEBOX_TASK_REQUEST: requestJson,
        HOMEBOY_WP_CODEBOX_SANDBOX_SESSION_ID: taskRequest.sandbox_session_id,
        HOMEBOY_WP_CODEBOX_GROUP_KEY: taskRequest.group_key,
      },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    let spawnError = null;
    let timedOut = false;
    let timeout = null;
    let forceKillTimeout = null;
    let killedProcessGroup = false;
    let forceKilledProcessGroup = false;
    const maxBuffer = options.max_buffer || 1024 * 1024 * 10;

    if (taskTimeoutSeconds > 0) {
      timeout = setTimeout(() => {
        timedOut = true;
        killedProcessGroup = killProcessTree(child, 'SIGTERM');
        forceKillTimeout = setTimeout(() => {
          forceKilledProcessGroup = killProcessTree(child, 'SIGKILL');
        }, 5000);
        forceKillTimeout.unref?.();
      }, taskTimeoutSeconds * 1000);
      timeout.unref?.();
    }

    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
      if (stdout.length > maxBuffer) {
        child.kill();
      }
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk;
      if (stderr.length > maxBuffer) {
        child.kill();
      }
    });
    child.on('error', (error) => {
      spawnError = error;
    });
    child.on('close', async (code, signal) => {
      if (timeout) {
        clearTimeout(timeout);
      }
      if (forceKillTimeout) {
        clearTimeout(forceKillTimeout);
      }
      const finishedAt = new Date().toISOString();
      const parsed = tryParseJson(stdout);
      const artifact = parsed && typeof parsed === 'object' && parsed.artifacts ? parsed.artifacts : null;
      const metrics = fanoutRecordMetrics(startedAt, finishedAt, parsed, artifact);
      const taskFailure = parsed ? wpCodeboxTaskFailure(parsed) : null;
      const timeoutError = timedOut ? `WP Codebox task timed out after ${taskTimeoutSeconds} seconds` : '';
      const errorMessage = timeoutError || (spawnError ? spawnError.message : (taskFailure || ''));
      const commandSuccess = code === 0 && !spawnError && null === taskFailure && !timedOut;
      const commandInfo = {
        bin: command,
        args,
        exit_code: code,
        signal: signal || null,
        error: errorMessage,
        timed_out: timedOut,
        timeout_seconds: taskTimeoutSeconds,
        killed_process_group: killedProcessGroup,
        force_killed_process_group: forceKilledProcessGroup,
      };
      const partialArtifacts = commandSuccess ? [] : await discoverTaskArtifactsAsync(args, taskRequest, startedAt, finishedAt, options);
      const outcome = enrichFailureOutcome(
        taskOutcome(taskRequest, parsed, artifact, commandSuccess, errorMessage, timedOut),
        taskRequest,
        commandInfo,
        startedAt,
        finishedAt,
        partialArtifacts
      );
      const success = taskOutcomeSucceeded(outcome);

      resolve({
        schema: RUN_SCHEMA,
        sandbox_session_id: taskRequest.sandbox_session_id,
        group_key: taskRequest.group_key,
        finding_id: taskRequest.finding_id || taskRequest.audit_findings[0]?.id || '',
        finding_ids: taskRequest.audit_findings.map((finding) => finding.id),
        command: commandInfo,
        status: success ? 'completed' : 'failed',
        started_at: startedAt,
        finished_at: finishedAt,
        stdout,
        stderr,
        result: parsed,
        outcome,
        artifact: artifact ? {
          id: artifact.id || '',
          directory: artifact.directory || artifact.path || '',
          preview_url: artifact.preview?.url || artifact.preview_url || '',
          runtime_reference_manifest: wpCodeboxRuntimeReferenceManifest(parsed, artifact),
        } : null,
        partial_artifacts: partialArtifacts,
        metrics,
      });
    });
    child.stdin.end(requestJson);
  });
}

function taskOutcome(taskRequest, parsed, artifact, success, errorMessage = '', timedOut = false) {
  const explicit = explicitWpCodeboxOutcome(parsed);
  if (isStructuredWpCodeboxOutcome(explicit)) {
    return structuredTaskOutcome(taskRequest, explicit, artifact, errorMessage);
  }

  const changedFiles = artifactChangedFiles(artifact);
  const hasPatch = artifactHasPatch(artifact);
  if (success && changedFiles.length > 0 && hasPatch) {
    const falsePositive = outputLooksFalsePositive(parsed);
    return structuredTaskOutcome(taskRequest, {
      kind: falsePositive ? 'false_positive_artifact' : 'fix_artifact',
      artifact: {
        id: artifact?.id || '',
        directory: artifact?.directory || artifact?.path || '',
        changed_files: changedFiles,
      },
      false_positive: falsePositive,
    }, artifact, errorMessage);
  }

  const urls = pullRequestUrls(parsed);
  const falsePositive = Boolean(
    explicit.kind === 'false_positive_pr' ||
    explicit.false_positive ||
    explicit.falsePositive ||
    explicit.false_positive_pr_url ||
    explicit.falsePositivePullRequestUrl ||
    explicit.disposition === 'false_positive'
  );
  const prUrl = explicit.pr_url || explicit.pull_request_url || explicit.pullRequestUrl || urls[0] || '';
  const falsePositivePrUrl = explicit.false_positive_pr_url || explicit.falsePositivePullRequestUrl || (falsePositive ? prUrl : '');
  let kind = 'explicit_failure';

  if (timedOut) {
    kind = 'timeout';
  } else if (falsePositive && falsePositivePrUrl) {
    kind = 'false_positive_pr';
  } else if (prUrl) {
    kind = 'fix_pr';
  }
  let failure = '';
  if (!success) {
    failure = errorMessage;
  } else if (kind === 'explicit_failure') {
    failure = 'WP Codebox task succeeded without an actionable patch, changed files, PR URL, or justified no-op outcome';
  }

  return {
    schema: 'homeboy/audit-wp-codebox-finding-outcome/v1',
    kind,
    finding_id: taskRequest.finding_id || taskRequest.audit_findings?.[0]?.id || '',
    finding_ids: taskRequest.audit_findings.map((finding) => finding.id),
    sandbox_session_id: taskRequest.sandbox_session_id,
    pr_url: prUrl,
    false_positive_pr_url: falsePositivePrUrl,
    false_positive: falsePositive,
    artifact_id: artifact?.id || '',
    failure,
    non_actionable: success && kind === 'explicit_failure',
    evidence: success && kind === 'explicit_failure' ? {
      artifact_id: artifact?.id || '',
      artifact_directory: artifact?.directory || artifact?.path || '',
      changed_file_count: changedFiles.length,
      has_patch: hasPatch,
      pr_url: prUrl,
      false_positive_pr_url: falsePositivePrUrl,
    } : undefined,
  };
}

function artifactChangedFiles(artifact) {
  const directory = artifact?.directory || artifact?.path || '';
  if (!directory) {
    return [];
  }
  const changedFilesPath = path.join(directory, 'files', 'changed-files.json');
  if (!fs.existsSync(changedFilesPath)) {
    return [];
  }
  const decoded = readJson(changedFilesPath);
  return (Array.isArray(decoded.files) ? decoded.files : []).map((file) => ({
    path: file.path || '',
    relative_path: file.relativePath || file.relative_path || '',
    status: file.status || '',
  })).filter((file) => file.path || file.relative_path);
}

function artifactHasPatch(artifact) {
  const directory = artifact?.directory || artifact?.path || '';
  return Boolean(directory && fs.existsSync(path.join(directory, 'files', 'patch.diff')));
}

function outputLooksFalsePositive(parsed) {
  const text = JSON.stringify(parsed || {}).toLowerCase();
  return text.includes('false positive') || text.includes('false_positive');
}

function explicitWpCodeboxOutcome(parsed) {
  if (!parsed || typeof parsed !== 'object') {
    return {};
  }
  if (parsed.outcome && typeof parsed.outcome === 'object') {
    return parsed.outcome;
  }
  if (parsed.result?.outcome && typeof parsed.result.outcome === 'object') {
    return parsed.result.outcome;
  }
  if (parsed.agent_runtime?.result?.outcome && typeof parsed.agent_runtime.result.outcome === 'object') {
    return parsed.agent_runtime.result.outcome;
  }
  if (parsed.output && typeof parsed.output === 'string') {
    const output = tryParseJsonFragment(parsed.output);
    const outcome = explicitWpCodeboxOutcome(output);
    if (isStructuredWpCodeboxOutcome(outcome)) {
      return outcome;
    }
  }
  for (const execution of Array.isArray(parsed.executions) ? parsed.executions : []) {
    for (const stream of ['stdout', 'stderr']) {
      const decoded = tryParseJsonFragment(execution?.[stream] || '');
      const outcome = explicitWpCodeboxOutcome(decoded);
      if (isStructuredWpCodeboxOutcome(outcome)) {
        return outcome;
      }
    }
  }
  if (parsed.result && typeof parsed.result === 'object') {
    return parsed.result;
  }
  return parsed;
}

function tryParseJsonFragment(value) {
  const text = String(value || '').trim();
  if (!text) {
    return null;
  }
  const direct = tryParseJson(text);
  if (direct) {
    return direct;
  }
  const start = text.indexOf('{');
  const end = text.lastIndexOf('}');
  if (start < 0 || end <= start) {
    return null;
  }
  return tryParseJson(text.slice(start, end + 1));
}

function isStructuredWpCodeboxOutcome(explicit) {
  return Boolean(explicit && typeof explicit === 'object' && WP_CODEBOX_STRUCTURED_OUTCOME_KINDS.has(explicit.kind));
}

function structuredTaskOutcome(taskRequest, explicit, artifact, errorMessage = '') {
  const urls = pullRequestUrls(explicit);
  const kind = explicit.kind === 'agent_no_pr_outcome' ? 'unable_to_remediate' : explicit.kind;
  const falsePositive = ['false_positive_artifact', 'false_positive_pr'].includes(kind) || Boolean(explicit.false_positive || explicit.falsePositive);
  const prUrl = explicit.pr_url || explicit.pull_request_url || explicit.pullRequestUrl || urls[0] || '';
  const falsePositivePrUrl = explicit.false_positive_pr_url || explicit.falsePositivePullRequestUrl || (falsePositive ? prUrl : '');
  const failure = explicit.failure || wpCodeboxOutcomeErrorMessage(explicit) || errorMessage || '';
  const retryable = explicit.retryable ?? explicit.provider_error?.retryable ?? explicit.error?.retryable;

  return {
    ...explicit,
    schema: 'homeboy/audit-wp-codebox-finding-outcome/v1',
    kind,
    finding_id: taskRequest.finding_id || taskRequest.audit_findings?.[0]?.id || '',
    finding_ids: taskRequest.audit_findings.map((finding) => finding.id),
    sandbox_session_id: taskRequest.sandbox_session_id,
    pr_url: prUrl,
    false_positive_pr_url: falsePositivePrUrl,
    false_positive: falsePositive,
    artifact_id: explicit.artifact_id || artifact?.id || '',
    failure,
    ...(retryable === undefined ? {} : { retryable: Boolean(retryable) }),
  };
}

function wpCodeboxOutcomeErrorMessage(explicit) {
  const candidates = [
    explicit.message,
    explicit.provider_message,
    explicit.provider_error?.message,
    explicit.error?.message,
    typeof explicit.error === 'string' ? explicit.error : '',
  ];
  return candidates.find((candidate) => typeof candidate === 'string' && candidate.trim()) || '';
}

function taskOutcomeSucceeded(outcome) {
  if (['fix_artifact', 'false_positive_artifact', 'noop_artifact', 'fix_pr', 'false_positive_pr'].includes(outcome?.kind)) {
    return true;
  }
  return outcome?.kind === 'unable_to_remediate' && hasJustifiedNoopOutcome(outcome);
}

function hasJustifiedNoopOutcome(outcome) {
  return [
    outcome?.justification,
    outcome?.noop_reason,
    outcome?.reason,
    outcome?.remediation_summary,
    outcome?.failure,
  ].some((value) => typeof value === 'string' && value.trim());
}

function pullRequestUrls(value, urls = []) {
  if (!value || urls.length >= 10) {
    return urls;
  }
  if (typeof value === 'string') {
    if (/^https:\/\/github\.com\/[^\s/]+\/[^\s/]+\/pull\/\d+/.test(value)) {
      urls.push(value);
    }
    return urls;
  }
  if (Array.isArray(value)) {
    value.forEach((entry) => pullRequestUrls(entry, urls));
    return urls;
  }
  if (typeof value === 'object') {
    Object.values(value).forEach((entry) => pullRequestUrls(entry, urls));
  }
  return Array.from(new Set(urls));
}

function killProcessTree(child, signal) {
  if (!child.pid) {
    return child.kill(signal);
  }

  try {
    process.kill(-child.pid, signal);
    return true;
  } catch {
    return child.kill(signal);
  }
}

function wpCodeboxTaskFailure(parsed) {
  if (!parsed || typeof parsed !== 'object') {
    return null;
  }

  if (parsed.success === false) {
    return wpCodeboxErrorMessage(parsed) || 'WP Codebox task reported success=false';
  }

  const executions = Array.isArray(parsed.executions) ? parsed.executions : [];
  for (const execution of executions) {
    const nested = tryParseJson(execution?.stdout || '');
    if (nested && typeof nested === 'object' && nested.success === false) {
      return wpCodeboxErrorMessage(nested) || 'WP Codebox nested execution reported success=false';
    }
  }

  return null;
}

function wpCodeboxErrorMessage(parsed) {
  const error = parsed && typeof parsed === 'object' ? parsed.error : null;
  if (typeof error === 'string') {
    return error;
  }
  if (error && typeof error === 'object' && typeof error.message === 'string') {
    return error.message;
  }
  return '';
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
    execute_task_request: (taskRequest) => executeWpCodeboxTaskRequestAsync(taskRequest, input),
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

function normalizeTaskTimeoutSeconds(value) {
  const parsed = Number.parseInt(value || DEFAULT_TASK_TIMEOUT_SECONDS, 10);
  if (!Number.isFinite(parsed) || parsed < 1) {
    return DEFAULT_TASK_TIMEOUT_SECONDS;
  }
  return parsed;
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
