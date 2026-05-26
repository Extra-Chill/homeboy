#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * Internal dependencies
 */
const { createAuditWpCodeboxFanoutPlanFromFiles } = require('../../lib/audit-wp-codebox-fanout');

function argValue(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function usage() {
  console.error('Usage: homeboy-audit-wp-codebox-fanout.cjs --audit-report <report.json> [--artifact-map <map.json>] [--output <plan.json>] [--issue-url <url>]');
  process.exit(1);
}

const auditReportPath = argValue('--audit-report');
if (!auditReportPath) {
  usage();
}

const plan = createAuditWpCodeboxFanoutPlanFromFiles({
  auditReportPath,
  artifactMapPath: argValue('--artifact-map') || '',
  outputPath: argValue('--output') || '',
  orchestratorId: argValue('--orchestrator-id') || undefined,
  runId: argValue('--run-id') || undefined,
  reportId: argValue('--report-id') || undefined,
  issueUrl: argValue('--issue-url') || undefined,
  base: argValue('--base') || undefined,
  branchPrefix: argValue('--branch-prefix') || undefined,
  reviewedAt: argValue('--reviewed-at') || undefined,
});

console.log(JSON.stringify(plan, null, 2));
