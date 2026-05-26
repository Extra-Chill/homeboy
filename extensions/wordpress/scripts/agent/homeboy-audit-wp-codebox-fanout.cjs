#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

/**
 * Internal dependencies
 */
const {
  createAuditWpCodeboxFanoutPlanFromFiles,
  executeAuditWpCodeboxFanoutFromFiles,
} = require('../../lib/audit-wp-codebox-fanout');

function argValue(name) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : '';
}

function argValues(name) {
  const values = [];
  for (let index = 0; index < process.argv.length; index += 1) {
    if (process.argv[index] === name && process.argv[index + 1]) {
      values.push(process.argv[index + 1]);
    }
  }
  return values;
}

function hasFlag(name) {
  return process.argv.includes(name);
}

function usage() {
  console.error('Usage: homeboy-audit-wp-codebox-fanout.cjs --audit-report <report.json> [--execute --wp-codebox-command <bin> --wp-codebox-arg <arg> --runs-output <run.json>] [--artifact-map <map.json>] [--output <plan.json>] [--issue-url <url>]');
  process.exit(1);
}

const auditReportPath = argValue('--audit-report');
if (!auditReportPath) {
  usage();
}

const options = {
  auditReportPath,
  artifactMapPath: argValue('--artifact-map') || '',
  outputPath: argValue('--output') || '',
  orchestratorId: argValue('--orchestrator-id') || undefined,
  runId: argValue('--run-id') || undefined,
  reportId: argValue('--report-id') || undefined,
  issueUrl: argValue('--issue-url') || undefined,
  provider: argValue('--provider') || undefined,
  model: argValue('--model') || undefined,
  providerPluginPaths: argValues('--provider-plugin-path'),
  secretEnv: argValues('--secret-env'),
  base: argValue('--base') || undefined,
  branchPrefix: argValue('--branch-prefix') || undefined,
  reviewedAt: argValue('--reviewed-at') || undefined,
};

const result = hasFlag('--execute') ? executeAuditWpCodeboxFanoutFromFiles({
  ...options,
  wpCodeboxCommand: argValue('--wp-codebox-command') || 'wp-codebox',
  wpCodeboxArgs: argValues('--wp-codebox-arg'),
  runsOutputPath: argValue('--runs-output') || '',
}) : createAuditWpCodeboxFanoutPlanFromFiles(options);

console.log(JSON.stringify(result, null, 2));
