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
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => { input += chunk; });
process.stdin.on('end', () => {
  const request = JSON.parse(input);
  if (request.group_key === 'docs-reference') {
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
  }));
});
`);
  return scriptPath;
}

const auditReportPath = path.join(__dirname, 'fixtures', 'homeboy-audit-wp-codebox-fanout', 'audit-report.json');
const report = readJson(auditReportPath);
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-audit-wp-codebox-fanout-'));

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
  assert.equal(initialPlan.audit.group_count, 2);
  assert.equal(initialPlan.task_requests.length, 2);

  assert.equal(safeBranchSlug('PHPCS Formatting/Auto Fix!'), 'phpcs-formatting-auto-fix');
  assert.equal(safeBranchSlug('foo..bar'), 'foo-bar');
  assert.equal(safeBranchSlug('foo/.bar'), 'foo-bar');
  assert.equal(safeBranchSlug('foo/bar.lock'), 'foo-bar-lock');
  assert.equal(safeBranchSlug('../.@{'), 'audit-batch');

  const phpcsRequest = initialPlan.task_requests.find((request) => request.group_key === 'PHPCS Formatting/Auto Fix!');
  const docsRequest = initialPlan.task_requests.find((request) => request.group_key === 'docs-reference');
  assert.equal(phpcsRequest.audit_findings.length, 2);
  assert.equal(docsRequest.audit_findings.length, 1);
  assert.match(phpcsRequest.sandbox_session_id, /^homeboy-audit-[a-f0-9]{16}$/);
  assert.equal(phpcsRequest.orchestrator.issue_url, 'https://github.com/Extra-Chill/homeboy-extensions/issues/769');
  assert.equal(phpcsRequest.provider, 'codex');
  assert.equal(phpcsRequest.model, 'gpt-5.5');
  assert.deepEqual(phpcsRequest.provider_plugin_paths, ['/opt/ai-provider-for-openai']);
  assert.deepEqual(phpcsRequest.secret_env, ['AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN']);
  assert.match(phpcsRequest.task.prompt, /finding-phpcs-001/);

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
    'PHPCS Formatting/Auto Fix!': {
      bundle_path: phpcsBundle.bundle,
      approved_files: [phpcsBundle.changedPath],
      reviewed_at: '2026-05-25T00:00:00.000Z',
      reviewer: 'homeboy-review-fixture',
      base: 'main',
    },
    'docs-reference': {
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
  assert.equal(plan.apply_back.length, 2);

  const phpcsApplyBack = plan.apply_back.find((entry) => entry.group_key === 'PHPCS Formatting/Auto Fix!');
  assert.equal(phpcsApplyBack.adapter_id, 'homeboy/wp-codebox-apply-adapter/v1');
  assert.equal(phpcsApplyBack.sandbox_session_id, phpcsRequest.sandbox_session_id);
  assert.deepEqual(phpcsApplyBack.finding_ids, ['finding-phpcs-001', 'finding-phpcs-002']);
  assert.equal(phpcsApplyBack.artifact.id, phpcsBundle.artifactId);
  assert.equal(phpcsApplyBack.artifact.content_digest, phpcsBundle.contentDigest);
  assert.equal(phpcsApplyBack.artifact.patch_sha256, phpcsBundle.patchSha256);
  assert.deepEqual(phpcsApplyBack.review.approved_files, [phpcsBundle.changedPath]);
  assert.equal(phpcsApplyBack.adapter_payload.bundlePath, fs.realpathSync(phpcsBundle.bundle));
  assert.equal(phpcsApplyBack.adapter_payload.branch, 'fix/homeboy-audit/phpcs-formatting-auto-fix');
  assert.equal(phpcsApplyBack.pull_request.base, 'main');
  assert.equal(phpcsApplyBack.pull_request.head, 'fix/homeboy-audit/phpcs-formatting-auto-fix');
  assert.match(phpcsApplyBack.pull_request.body, /Extra-Chill\/homeboy-extensions\/issues\/769/);

  const missingApprovalMap = {
    'PHPCS Formatting/Auto Fix!': {
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
    artifact_map: {
      ...artifactMap,
      'docs-reference': {
        bundle_path: docsBundle.bundle,
        approved_files: [docsBundle.changedPath],
        approved: false,
      },
    },
  });
  assert.equal(rejectedPlan.apply_back.length, 1);
  assert.equal(rejectedPlan.apply_back[0].group_key, 'PHPCS Formatting/Auto Fix!');

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
  assert.equal(filePlan.apply_back.length, 2);
  assert.equal(readJson(outputPath).apply_back.length, 2);

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
  assert.equal(cliPlan.task_requests.length, 2);
  assert.equal(cliPlan.apply_back.length, 2);
  assert.equal(cliPlan.task_requests[0].provider, 'codex');

  const fixtureCommand = createWpCodeboxFixtureCommand(root);
  const execution = executeAuditWpCodeboxFanout({
    report,
    issue_url: 'https://github.com/Extra-Chill/homeboy-extensions/issues/773',
    provider: 'codex',
    model: 'gpt-5.5',
    wp_codebox_command: process.execPath,
    wp_codebox_args: [fixtureCommand],
  });
  assert.equal(execution.schema, 'homeboy/audit-wp-codebox-execution/v1');
  assert.equal(execution.records.length, 2);
  assert.equal(execution.status, 'failed');
  const completedRecord = execution.records.find((record) => record.group_key === 'PHPCS Formatting/Auto Fix!');
  const failedRecord = execution.records.find((record) => record.group_key === 'docs-reference');
  assert.equal(completedRecord.status, 'completed');
  assert.equal(completedRecord.command.bin, process.execPath);
  assert.equal(completedRecord.result.session.id, completedRecord.sandbox_session_id);
  assert.equal(completedRecord.result.session.orchestrator.issue_url, 'https://github.com/Extra-Chill/homeboy-extensions/issues/773');
  assert.match(completedRecord.artifact.id, /^artifact-homeboy-audit-/);
  assert.equal(failedRecord.status, 'failed');
  assert.equal(failedRecord.command.exit_code, 3);
  assert.match(failedRecord.stderr, /fixture docs-reference failure/);

  const runsOutputPath = path.join(root, 'fanout-run.json');
  const fileExecution = executeAuditWpCodeboxFanoutFromFiles({
    auditReportPath,
    issueUrl: 'https://github.com/Extra-Chill/homeboy-extensions/issues/773',
    wpCodeboxCommand: process.execPath,
    wpCodeboxArgs: [fixtureCommand],
    runsOutputPath,
  });
  assert.equal(fileExecution.records.length, 2);
  assert.equal(readJson(runsOutputPath).records.length, 2);

  const cliExecutionOutput = run(process.execPath, [
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
  ]);
  const cliExecution = JSON.parse(cliExecutionOutput);
  assert.equal(cliExecution.records.length, 2);
  assert.equal(cliExecution.records[0].schema, 'homeboy/audit-wp-codebox-run/v1');

  console.log('Homeboy audit WP Codebox fanout smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
