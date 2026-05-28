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
  console.error('Usage: homeboy-audit-wp-codebox-fanout.cjs --audit-report <report.json> [--execute --concurrency <n> --task-timeout-seconds <n> --wp-codebox-command <bin> --wp-codebox-arg <arg> --runs-output <run.json>] [--artifact-map <map.json>] [--output <plan.json>] [--issue-url <url>]');
  process.exit(1);
}

function writeProgress(event) {
  const elapsed = event.elapsed_ms === null ? '' : ` elapsed=${event.elapsed_ms}ms`;
  const artifact = event.artifact_directory ? ` artifact=${event.artifact_directory}` : '';
  const outcome = event.outcome_kind ? ` outcome=${event.outcome_kind}` : '';
  const retryable = event.retryable === null ? '' : ` retryable=${event.retryable ? 'yes' : 'no'}`;
  const failure = event.failure ? ` failure=${event.failure}` : '';
  process.stderr.write([
    '[homeboy wp-codebox fanout]',
    event.status,
    `${event.group_index}/${event.group_count}`,
    `group=${event.group_key}`,
    `session=${event.sandbox_session_id}`,
    `${elapsed}${artifact}${outcome}${retryable}${failure}`.trim(),
  ].filter(Boolean).join(' ') + '\n');
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

async function main() {
  const result = hasFlag('--execute') ? await executeAuditWpCodeboxFanoutFromFiles({
    ...options,
    wpCodeboxCommand: argValue('--wp-codebox-command') || 'wp-codebox',
    wpCodeboxArgs: argValues('--wp-codebox-arg'),
    runsOutputPath: argValue('--runs-output') || '',
    concurrency: argValue('--concurrency') || undefined,
    taskTimeoutSeconds: argValue('--task-timeout-seconds') || undefined,
    onProgress: writeProgress,
  }) : createAuditWpCodeboxFanoutPlanFromFiles(options);

  console.log(JSON.stringify(result, null, 2));
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
