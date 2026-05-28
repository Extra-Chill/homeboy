'use strict';

const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
  createAuditWpCodeboxFanoutPlan,
  createAuditWpCodeboxFanoutPlanFromFiles,
  executeAuditWpCodeboxFanout,
  executeAuditWpCodeboxFanoutFromFiles,
  safeBranchSlug,
  taskOutcome,
} = require('../lib/audit-wp-codebox-fanout');
const { artifactContentDigest } = require('../lib/wp-codebox-apply-adapter');

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

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: options.cwd, encoding: 'utf8' });
  assert.equal(result.status, 0, `${command} ${args.join(' ')} failed: ${result.stderr || result.stdout}`);
  return (result.stdout || '').trim();
}

function createBundle(root, name, changedPath, relativePath) {
  const bundle = path.join(root, name);
  const files = path.join(bundle, 'files');
  fs.mkdirSync(files, { recursive: true });

  const changedFiles = {
    schema: 'wp-codebox/changed-files/v1',
    files: [
      {
        path: changedPath,
        status: 'modified',
        mountTarget: '/wordpress/wp-content/plugins/fixture-plugin',
        relativePath,
      },
    ],
  };
  const changedFilesJson = `${JSON.stringify(changedFiles, null, 2)}\n`;
  const patch = [
    `diff --git a/wordpress/wp-content/plugins/fixture-plugin/${relativePath} b/wordpress/wp-content/plugins/fixture-plugin/${relativePath}`,
    `--- a/wordpress/wp-content/plugins/fixture-plugin/${relativePath}`,
    `+++ b/wordpress/wp-content/plugins/fixture-plugin/${relativePath}`,
    '@@ -1,1 +1,1 @@',
    '-before',
    '+after',
    '',
  ].join('\n');
  const contentDigest = artifactContentDigest(changedFilesJson, patch);
  const artifactId = `artifact-bundle-sha256-${contentDigest}`;
  const contentDigestMetadata = {
    algorithm: 'sha256',
    inputs: ['files/changed-files.json', 'files/patch.diff'],
    value: contentDigest,
  };

  writeJson(path.join(bundle, 'manifest.json'), {
    id: artifactId,
    contentDigest: contentDigestMetadata,
    files: [
      { path: 'files/changed-files.json', kind: 'changed-files' },
      { path: 'files/patch.diff', kind: 'patch' },
      { path: 'files/review.json', kind: 'review' },
    ],
  });
  writeJson(path.join(bundle, 'metadata.json'), {
    id: artifactId,
    contentDigest: contentDigestMetadata,
    artifacts: { changedFiles: 'files/changed-files.json', patch: 'files/patch.diff' },
  });
  writeJson(path.join(files, 'changed-files.json'), changedFiles);
  fs.writeFileSync(path.join(files, 'patch.diff'), patch);
  writeJson(path.join(files, 'review.json'), {
    schema: 'wp-codebox/artifact-review/v1',
    artifactId,
    summary: `Fixture review for ${name}`,
    evidence: {
      patch: 'files/patch.diff',
      patchSha256: sha256(patch),
      artifactContentDigest: contentDigest,
      changedFiles: 'files/changed-files.json',
    },
  });

  return { artifactId, bundle, changedPath, contentDigest, patchSha256: sha256(patch) };
}

function createWpCodeboxFixtureCommand(root) {
  const scriptPath = path.join(root, 'fixture-wp-codebox.cjs');
  fs.writeFileSync(scriptPath, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => { input += chunk; });
process.stdin.on('end', () => {
  const request = JSON.parse(input);
  if (process.env.FIXTURE_HANG_GROUP === request.group_key) {
    process.stdout.write('fixture partial stdout before timeout\\n');
    process.stderr.write('fixture partial stderr before timeout\\n');
    setInterval(() => {}, 1000);
    return;
  }
  if (request.group_key === 'finding-doc-001') {
    if (process.env.FIXTURE_EXPECT_INCREMENTAL_RUN) {
      const run = JSON.parse(fs.readFileSync(process.env.FIXTURE_EXPECT_INCREMENTAL_RUN, 'utf8'));
      if (run.status !== 'incomplete' || run.records.length !== 1 || !run.current_group || run.current_group.group_key !== request.group_key) {
        process.stderr.write('fixture incremental fanout-run summary missing expected partial state\\n');
        process.exit(4);
      }
    }
    if (process.env.FIXTURE_NESTED_WP_ERROR) {
      process.stdout.write(JSON.stringify({
        success: true,
        executions: [
          {
            command: 'wordpress.run-php',
            stdout: JSON.stringify({
              success: false,
              error: { code: 'fixture_wp_error', message: 'Fixture nested WP error' },
            }),
          },
        ],
      }));
      process.exit(0);
    }
    process.stderr.write('fixture docs-reference failure\\n');
    process.exit(3);
  }
  process.stdout.write(JSON.stringify({
    success: true,
    session: {
      schema: 'wp-codebox/sandbox-session/v1',
      id: request.sandbox_session_id,
      orchestrator: request.orchestrator,
    },
    artifacts: {
      id: 'artifact-' + request.sandbox_session_id,
      directory: '/tmp/' + request.sandbox_session_id,
      preview: { url: 'https://preview.example.test/' + request.sandbox_session_id },
    },
    outcome: {
      pr_url: 'https://github.com/Extra-Chill/homeboy-extensions/pull/' + (request.finding_id === 'finding-phpcs-002' ? '778' : '777')
    },
  }));
});
`);
  return scriptPath;
}

const auditReportPath = path.join(__dirname, 'fixtures', 'homeboy-audit-wp-codebox-fanout', 'audit-report.json');
const report = readJson(auditReportPath);
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-audit-wp-codebox-fanout-'));

async function main() {
try {
  const initialPlan = createAuditWpCodeboxFanoutPlan({
    report,
    issue_url: 'https://github.com/Extra-Chill/homeboy-extensions/issues/769',
    provider: 'codex',
    model: 'gpt-5.5',
    provider_plugin_paths: ['/opt/ai-provider-for-openai'],
    secret_env: ['AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN'],
  });
  assert.equal(initialPlan.schema, 'homeboy/audit-wp-codebox-fanout/v1');
  assert.equal(initialPlan.audit.finding_count, 3);
  assert.equal(initialPlan.audit.group_count, 3);
  assert.equal(initialPlan.audit.task_count, 3);
  assert.equal(initialPlan.task_requests.length, 3);

  assert.equal(safeBranchSlug('PHPCS Formatting/Auto Fix!'), 'phpcs-formatting-auto-fix');
  assert.equal(safeBranchSlug('foo..bar'), 'foo-bar');
  assert.equal(safeBranchSlug('foo/.bar'), 'foo-bar');
  assert.equal(safeBranchSlug('foo/bar.lock'), 'foo-bar-lock');
  assert.equal(safeBranchSlug('../.@{'), 'audit-batch');

  const phpcsRequest = initialPlan.task_requests.find((request) => request.group_key === 'finding-phpcs-001');
  const docsRequest = initialPlan.task_requests.find((request) => request.group_key === 'finding-doc-001');
  assert.equal(phpcsRequest.audit_findings.length, 1);
  assert.equal(docsRequest.audit_findings.length, 1);
  assert.equal(phpcsRequest.finding_id, 'finding-phpcs-001');
  assert.match(phpcsRequest.sandbox_session_id, /^homeboy-audit-[a-f0-9]{16}$/);
  assert.equal(phpcsRequest.orchestrator.issue_url, 'https://github.com/Extra-Chill/homeboy-extensions/issues/769');
  assert.equal(phpcsRequest.provider, 'codex');
  assert.equal(phpcsRequest.model, 'gpt-5.5');
  assert.deepEqual(phpcsRequest.provider_plugin_paths, ['/opt/ai-provider-for-openai']);
  assert.deepEqual(phpcsRequest.secret_env, ['AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN']);
  assert.match(phpcsRequest.task.prompt, /finding-phpcs-001/);
  assert.match(phpcsRequest.task.prompt, /false positive/);
  const falsePositiveOutcome = taskOutcome(phpcsRequest, {
    outcome: {
      kind: 'false_positive_pr',
      false_positive_pr_url: 'https://github.com/Extra-Chill/homeboy-extensions/pull/779',
    },
  }, null, true);
  assert.equal(falsePositiveOutcome.kind, 'false_positive_pr');
  assert.equal(falsePositiveOutcome.false_positive_pr_url, 'https://github.com/Extra-Chill/homeboy-extensions/pull/779');

  const phpcsBundle = createBundle(
    root,
    'phpcs-bundle',
    '/wordpress/wp-content/plugins/fixture-plugin/src/Example/class-alpha.php',
    'src/Example/class-alpha.php'
  );
  const docsBundle = createBundle(
    root,
    'docs-bundle',
    '/wordpress/wp-content/plugins/fixture-plugin/docs/setup.md',
    'docs/setup.md'
  );
  const artifactMap = {
    'finding-phpcs-001': {
      bundle_path: phpcsBundle.bundle,
      approved_files: [phpcsBundle.changedPath],
      reviewed_at: '2026-05-25T00:00:00.000Z',
      reviewer: 'homeboy-review-fixture',
      base: 'main',
    },
    'finding-phpcs-002': {
      bundle_path: phpcsBundle.bundle,
      approved_files: [phpcsBundle.changedPath],
      reviewed_at: '2026-05-25T00:00:00.000Z',
      reviewer: 'homeboy-review-fixture',
      base: 'main',
    },
    'finding-doc-001': {
      bundle_path: docsBundle.bundle,
      approved_files: [docsBundle.changedPath],
      reviewed_at: '2026-05-25T00:00:00.000Z',
      reviewer: 'homeboy-review-fixture',
      base: 'main',
    },
  };

  const plan = createAuditWpCodeboxFanoutPlan({
    report,
    artifact_map: artifactMap,
    issue_url: 'https://github.com/Extra-Chill/homeboy-extensions/issues/769',
  });
  assert.equal(plan.apply_back.length, 3);
  const phpcsApplyBack = plan.apply_back.find((entry) => entry.group_key === 'finding-phpcs-001');
  assert.equal(phpcsApplyBack.adapter_id, 'homeboy/wp-codebox-apply-adapter/v1');
  assert.equal(phpcsApplyBack.sandbox_session_id, phpcsRequest.sandbox_session_id);
  assert.deepEqual(phpcsApplyBack.finding_ids, ['finding-phpcs-001']);
  assert.equal(phpcsApplyBack.artifact.id, phpcsBundle.artifactId);
  assert.equal(phpcsApplyBack.artifact.content_digest, phpcsBundle.contentDigest);
  assert.equal(phpcsApplyBack.artifact.patch_sha256, phpcsBundle.patchSha256);
  assert.deepEqual(phpcsApplyBack.review.approved_files, [phpcsBundle.changedPath]);
  assert.equal(phpcsApplyBack.adapter_payload.bundlePath, fs.realpathSync(phpcsBundle.bundle));
  assert.equal(phpcsApplyBack.adapter_payload.branch, 'fix/homeboy-audit/finding-phpcs-001');
  assert.equal(phpcsApplyBack.pull_request.base, 'main');
  assert.equal(phpcsApplyBack.pull_request.head, 'fix/homeboy-audit/finding-phpcs-001');
  assert.match(phpcsApplyBack.pull_request.body, /Extra-Chill\/homeboy-extensions\/issues\/769/);

  const missingApprovalMap = {
    'finding-phpcs-001': {
      bundle_path: phpcsBundle.bundle,
    },
  };
  assert.throws(
    () => createAuditWpCodeboxFanoutPlan({
      report,
      artifact_map: missingApprovalMap,
    }),
    /non-empty explicit approved_files/
  );

  const rejectedPlan = createAuditWpCodeboxFanoutPlan({
    report,
    issue_url: 'https://github.com/Extra-Chill/homeboy-extensions/issues/769',
    artifact_map: {
      ...artifactMap,
      'finding-doc-001': {
        bundle_path: docsBundle.bundle,
        approved_files: [docsBundle.changedPath],
        approved: false,
        false_positive: true,
        reason: 'Fixture docs-reference rule is intentionally noisy.',
      },
    },
  });
  assert.equal(rejectedPlan.apply_back.length, 2);
  assert.equal(rejectedPlan.apply_back[0].group_key, 'finding-phpcs-001');
  assert.equal(rejectedPlan.issue_reports.length, 1);
  assert.equal(rejectedPlan.issue_reports[0].schema, 'homeboy/audit-wp-codebox-issue-report/v1');
  assert.equal(rejectedPlan.issue_reports[0].group_key, 'finding-doc-001');
  assert.equal(rejectedPlan.issue_reports[0].disposition, 'false_positive');
  assert.deepEqual(rejectedPlan.issue_reports[0].finding_ids, ['finding-doc-001']);
  assert.equal(rejectedPlan.issue_reports[0].artifact.id, docsBundle.artifactId);
  assert.match(rejectedPlan.issue_reports[0].issue.title, /false positive/);
  assert.match(rejectedPlan.issue_reports[0].issue.body, /Fixture docs-reference rule is intentionally noisy/);
  assert.match(rejectedPlan.issue_reports[0].issue.body, /Extra-Chill\/homeboy-extensions\/issues\/769/);

  const artifactMapPath = path.join(root, 'artifact-map.json');
  const outputPath = path.join(root, 'fanout-plan.json');
  writeJson(artifactMapPath, artifactMap);
  const filePlan = createAuditWpCodeboxFanoutPlanFromFiles({
    auditReportPath,
    artifactMapPath,
    outputPath,
    issueUrl: 'https://github.com/Extra-Chill/homeboy-extensions/issues/769',
    provider: 'codex',
    model: 'gpt-5.5',
    providerPluginPaths: ['/opt/ai-provider-for-openai'],
    secretEnv: ['AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN'],
  });
  assert.equal(filePlan.apply_back.length, 3);
  assert.equal(readJson(outputPath).apply_back.length, 3);

  const cliOutput = run(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-audit-wp-codebox-fanout.cjs'),
    '--audit-report',
    auditReportPath,
    '--artifact-map',
    artifactMapPath,
    '--issue-url',
    'https://github.com/Extra-Chill/homeboy-extensions/issues/769',
    '--provider',
    'codex',
    '--model',
    'gpt-5.5',
    '--provider-plugin-path',
    '/opt/ai-provider-for-openai',
    '--secret-env',
    'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  ]);
  const cliPlan = JSON.parse(cliOutput);
  assert.equal(cliPlan.task_requests.length, 3);
  assert.equal(cliPlan.apply_back.length, 3);
  assert.equal(cliPlan.task_requests[0].provider, 'codex');

  const fixtureCommand = createWpCodeboxFixtureCommand(root);
  const execution = await executeAuditWpCodeboxFanout({
    report,
    issue_url: 'https://github.com/Extra-Chill/homeboy-extensions/issues/773',
    provider: 'codex',
    model: 'gpt-5.5',
    wp_codebox_command: process.execPath,
    wp_codebox_args: [fixtureCommand],
  });
  assert.equal(execution.schema, 'homeboy/audit-wp-codebox-execution/v1');
  assert.equal(execution.records.length, 3);
  assert.equal(execution.status, 'failed');
  const completedRecord = execution.records.find((record) => record.group_key === 'finding-phpcs-001');
  const failedRecord = execution.records.find((record) => record.group_key === 'finding-doc-001');
  assert.equal(completedRecord.status, 'completed');
  assert.equal(completedRecord.finding_id, 'finding-phpcs-001');
  assert.equal(completedRecord.command.bin, process.execPath);
  assert.equal(completedRecord.result.session.id, completedRecord.sandbox_session_id);
  assert.equal(completedRecord.result.session.orchestrator.issue_url, 'https://github.com/Extra-Chill/homeboy-extensions/issues/773');
  assert.match(completedRecord.artifact.id, /^artifact-homeboy-audit-/);
  assert.equal(completedRecord.outcome.kind, 'fix_pr');
  assert.equal(completedRecord.outcome.pr_url, 'https://github.com/Extra-Chill/homeboy-extensions/pull/777');
  assert.equal(execution.outcomes.length, 3);
  assert.equal(failedRecord.status, 'failed');
  assert.equal(failedRecord.command.exit_code, 3);
  assert.equal(failedRecord.outcome.kind, 'explicit_failure');
  assert.match(failedRecord.stderr, /fixture docs-reference failure/);

  const nestedErrorExecution = await executeAuditWpCodeboxFanout({
    report,
    wp_codebox_command: process.execPath,
    wp_codebox_args: [fixtureCommand],
    env: { FIXTURE_NESTED_WP_ERROR: '1' },
  });
  const nestedFailedRecord = nestedErrorExecution.records.find((record) => record.group_key === 'finding-doc-001');
  assert.equal(nestedErrorExecution.status, 'failed');
  assert.equal(nestedFailedRecord.status, 'failed');
  assert.equal(nestedFailedRecord.command.exit_code, 0);
  assert.match(nestedFailedRecord.command.error, /Fixture nested WP error/);

  const timeoutRunsOutputPath = path.join(root, 'fanout-timeout-run.json');
  const timeoutExecution = await executeAuditWpCodeboxFanout({
    report,
    wp_codebox_command: process.execPath,
    wp_codebox_args: [fixtureCommand],
    concurrency: 1,
    task_timeout_seconds: 1,
    runsOutputPath: timeoutRunsOutputPath,
    env: { FIXTURE_HANG_GROUP: 'finding-phpcs-001' },
  });
  assert.equal(timeoutExecution.status, 'failed');
  assert.equal(timeoutExecution.records.length, 3);
  const timeoutRecord = timeoutExecution.records.find((record) => record.group_key === 'finding-phpcs-001');
  assert.equal(timeoutRecord.status, 'failed');
  assert.equal(timeoutRecord.command.timed_out, true);
  assert.equal(timeoutRecord.command.timeout_seconds, 1);
  assert.equal(timeoutRecord.command.killed_process_group, true);
  assert.match(timeoutRecord.command.error, /timed out after 1 seconds/);
  assert.equal(timeoutRecord.outcome.kind, 'timeout');
  assert.match(timeoutRecord.stdout, /fixture partial stdout before timeout/);
  assert.match(timeoutRecord.stderr, /fixture partial stderr before timeout/);
  const timeoutFollowupRecord = timeoutExecution.records.find((record) => record.group_key === 'finding-doc-001');
  assert.equal(timeoutFollowupRecord.status, 'failed');
  assert.equal(timeoutFollowupRecord.command.exit_code, 3);
  const timeoutFinalRun = readJson(timeoutRunsOutputPath);
  assert.equal(timeoutFinalRun.status, 'failed');
  assert.equal(timeoutFinalRun.records.length, 3);
  assert.equal(timeoutFinalRun.outcomes.length, 3);
  assert.equal(Object.hasOwn(timeoutFinalRun, 'current_group'), false);

  const runsOutputPath = path.join(root, 'fanout-run.json');
  const fileExecution = await executeAuditWpCodeboxFanoutFromFiles({
    auditReportPath,
    issueUrl: 'https://github.com/Extra-Chill/homeboy-extensions/issues/773',
    wpCodeboxCommand: process.execPath,
    wpCodeboxArgs: [fixtureCommand],
    concurrency: 1,
    runsOutputPath,
    env: {
      FIXTURE_EXPECT_INCREMENTAL_RUN: runsOutputPath,
    },
  });
  assert.equal(fileExecution.records.length, 3);
  const finalRun = readJson(runsOutputPath);
  assert.equal(finalRun.records.length, 3);
  assert.equal(Object.hasOwn(finalRun, 'current_group'), false);

  const cliExecutionResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-audit-wp-codebox-fanout.cjs'),
    '--audit-report',
    auditReportPath,
    '--issue-url',
    'https://github.com/Extra-Chill/homeboy-extensions/issues/773',
    '--execute',
    '--wp-codebox-command',
    process.execPath,
    '--wp-codebox-arg',
    fixtureCommand,
    '--task-timeout-seconds',
    '2',
    '--secret-env',
    'FIXTURE_SECRET_TOKEN',
  ], { encoding: 'utf8' });
  assert.equal(cliExecutionResult.status, 0, cliExecutionResult.stderr || cliExecutionResult.stdout);
  const cliExecution = JSON.parse(cliExecutionResult.stdout);
  assert.equal(cliExecution.records.length, 3);
  assert.equal(cliExecution.records[0].schema, 'homeboy/audit-wp-codebox-run/v1');
  assert.match(cliExecutionResult.stderr, /\[homeboy wp-codebox fanout\] started 1\/3 group=finding-phpcs-001 session=homeboy-audit-/);
  assert.match(cliExecutionResult.stderr, /\[homeboy wp-codebox fanout\] completed 1\/3 group=finding-phpcs-001 session=homeboy-audit-.*artifact=\/tmp\/homeboy-audit-/);
  assert.match(cliExecutionResult.stderr, /\[homeboy wp-codebox fanout\] failed 3\/3 group=finding-doc-001 session=homeboy-audit-/);
  assert.doesNotMatch(cliExecutionResult.stderr, /FIXTURE_SECRET_TOKEN/);

  console.log('Homeboy audit WP Codebox fanout smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
