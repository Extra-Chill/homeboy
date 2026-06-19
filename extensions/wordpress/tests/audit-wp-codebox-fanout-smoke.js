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
  IMPLEMENTATION_SCOPE,
  safeBranchSlug,
  taskOutcome,
  taskOutcomeSucceeded,
} = require('../lib/audit-wp-codebox-fanout');
const {
  auditFanoutRuntimeInvocation,
} = require('../lib/audit-fanout-runtime-adapter');
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

function assertMetrics(record, options = {}) {
  assert.ok(record.metrics, `missing metrics for ${record.group_key}`);
  assert.equal(typeof record.metrics.duration_ms, 'number');
  assert.ok(record.metrics.duration_ms >= 0);
  assert.ok(Object.hasOwn(record.metrics, 'peak_rss_bytes'));
  assert.equal(typeof record.metrics.sample_count, 'number');
  assert.ok(Object.hasOwn(record.metrics, 'child_process_count_peak'));
  assert.ok(Object.hasOwn(record.metrics, 'artifact_bytes'));
  if (options.runnerMetrics) {
    assert.equal(record.metrics.peak_rss_bytes, 123456);
    assert.equal(record.metrics.sample_count, 3);
    assert.equal(record.metrics.child_process_count_peak, 2);
    assert.equal(record.metrics.cpu_user_ms, 12);
    assert.equal(record.metrics.cpu_system_ms, 4);
    assert.equal(record.metrics.source, 'linux_procfs_process_tree');
  }
  if (options.artifactBytes) {
    assert.equal(typeof record.metrics.artifact_bytes, 'number');
    assert.ok(record.metrics.artifact_bytes > 0);
  }
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
  const patch = [
    `diff --git a/wordpress/wp-content/plugins/fixture-plugin/${relativePath} b/wordpress/wp-content/plugins/fixture-plugin/${relativePath}`,
    `--- a/wordpress/wp-content/plugins/fixture-plugin/${relativePath}`,
    `+++ b/wordpress/wp-content/plugins/fixture-plugin/${relativePath}`,
    '@@ -1,1 +1,1 @@',
    '-before',
    '+after',
    '',
  ].join('\n');
  const contentDigest = sha256(`fixture-artifact:${name}:${changedPath}:${relativePath}`);
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
const path = require('node:path');
const root = ${JSON.stringify(root)};
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => { input += chunk; });
process.stdin.on('end', () => {
  const request = JSON.parse(input);
  const artifactsIndex = process.argv.indexOf('--artifacts');
  const artifactsRoot = artifactsIndex >= 0 ? process.argv[artifactsIndex + 1] : '';
  function writePartialArtifact() {
    if (!artifactsRoot) {
      return '';
    }
    const artifactDirectory = path.join(artifactsRoot, 'partial-' + request.sandbox_session_id);
    const filesDirectory = path.join(artifactDirectory, 'files');
    fs.mkdirSync(filesDirectory, { recursive: true });
    fs.writeFileSync(path.join(filesDirectory, 'changed-files.json'), JSON.stringify({
      schema: 'wp-codebox/changed-files/v1',
      files: [{ path: '/workspace/homeboy-extensions/partial.php', relativePath: 'partial.php', status: 'modified' }],
    }, null, 2) + '\\n');
    fs.writeFileSync(path.join(filesDirectory, 'runtime-reference-manifest.json'), JSON.stringify({
      schema: 'wp-codebox/runtime-reference-manifest-fixture/v1',
      cookie: 'partial-cookie-secret',
    }, null, 2) + '\\n');
    return artifactDirectory;
  }
  if (process.env.FIXTURE_HANG_GROUP === request.group_key) {
    writePartialArtifact();
    process.stdout.write('fixture partial stdout before timeout\\n');
    process.stderr.write('fixture partial stderr before timeout\\n');
    setInterval(() => {}, 1000);
    return;
  }
  if (process.env.FIXTURE_PROVIDER_ERROR_GROUP === request.group_key) {
    process.stdout.write(JSON.stringify({
      success: false,
      outcome: {
        kind: 'provider_error',
        retryable: true,
        provider: 'openai',
        provider_error: {
          code: 'rate_limit_exceeded',
          message: 'Fixture provider returned 429',
          retryable: true,
        },
        metadata: {
          datamachine: {
            completed: false,
            max_turns_reached: false,
          },
        },
      },
    }));
    process.exit(2);
  }
  if (process.env.FIXTURE_NOOP_GROUP === request.group_key) {
    process.stdout.write(JSON.stringify({
      success: true,
      outcome: {
        kind: 'noop_artifact',
        false_positive: true,
      },
      metrics: {
        duration_ms: 99,
        peak_rss_bytes: 123456,
        sample_count: 3,
        child_process_count_peak: 2,
        cpu_user_ms: 12,
        cpu_system_ms: 4,
        source: 'linux_procfs_process_tree',
      },
    }));
    process.exit(0);
  }
  if (process.env.FIXTURE_NO_ACTIONABLE_GROUP === request.group_key) {
    const artifactDirectory = path.join(root, 'no-actionable-' + request.sandbox_session_id);
    fs.mkdirSync(artifactDirectory, { recursive: true });
    fs.writeFileSync(path.join(artifactDirectory, 'artifact.txt'), 'fixture non-actionable artifact bytes\\n');
    process.stdout.write(JSON.stringify({
      success: true,
      artifacts: {
        id: 'artifact-no-actionable-' + request.sandbox_session_id,
        directory: artifactDirectory,
      },
      metrics: {
        duration_ms: 99,
        peak_rss_bytes: 123456,
        sample_count: 3,
        child_process_count_peak: 2,
        cpu_user_ms: 12,
        cpu_system_ms: 4,
        source: 'linux_procfs_process_tree',
      },
    }));
    process.exit(0);
  }
  if (request.group_key === 'docs-reference') {
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
  const artifactDirectory = path.join(root, 'artifact-' + request.sandbox_session_id);
  const filesDirectory = path.join(artifactDirectory, 'files');
  fs.mkdirSync(artifactDirectory, { recursive: true });
  fs.mkdirSync(filesDirectory, { recursive: true });
  fs.writeFileSync(path.join(artifactDirectory, 'artifact.txt'), 'fixture artifact bytes\\n');
  fs.writeFileSync(path.join(filesDirectory, 'runtime-reference-manifest.json'), JSON.stringify({
    schema: 'wp-codebox/runtime-reference-manifest-fixture/v1',
    runtime: { id: request.sandbox_session_id, provider: 'wordpress-playground' },
    references: [{ kind: 'workspace', digest: 'sha256:fixture-reference' }],
    cookie: 'fixture-cookie-secret',
  }, null, 2) + '\\n');
  process.stdout.write(JSON.stringify({
    success: true,
    session: {
      schema: 'wp-codebox/sandbox-session/v1',
      id: request.sandbox_session_id,
      orchestrator: request.orchestrator,
    },
    artifacts: {
      id: 'artifact-' + request.sandbox_session_id,
      directory: artifactDirectory,
      preview: { url: 'https://preview.example.test/' + request.sandbox_session_id },
      runtimeReferenceManifestPath: path.join(filesDirectory, 'runtime-reference-manifest.json'),
    },
    metrics: {
      duration_ms: 99,
      peak_rss_bytes: 123456,
      sample_count: 3,
      child_process_count_peak: 2,
      cpu_user_ms: 12,
      cpu_system_ms: 4,
      source: 'linux_procfs_process_tree',
    },
    outcome: {
      pr_url: 'https://github.com/Extra-Chill/homeboy-extensions/pull/' + (request.group_key === 'docs-reference' ? '778' : '777')
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
  assert.equal(IMPLEMENTATION_SCOPE.quarantine, 'wp-codebox-compatibility-entrypoint');
  assert.equal(IMPLEMENTATION_SCOPE.generic_surface, false);
  assert.equal(IMPLEMENTATION_SCOPE.runtime_adapter, 'wordpress/lib/audit-fanout-runtime-adapter.js');
  assert.ok(IMPLEMENTATION_SCOPE.public_entrypoints.includes('wordpress/lib/audit-wp-codebox-fanout.js'));

  const defaultRuntimeInvocation = auditFanoutRuntimeInvocation();
  assert.equal(defaultRuntimeInvocation.runtime.id, 'wp-codebox');
  assert.equal(defaultRuntimeInvocation.command, 'wp-codebox');
  const explicitRuntimeInvocation = auditFanoutRuntimeInvocation({
    wp_codebox_command: process.execPath,
    wp_codebox_args: ['fixture-command.cjs'],
  });
  assert.equal(explicitRuntimeInvocation.runtime.id, 'wp-codebox');
  assert.equal(explicitRuntimeInvocation.command, process.execPath);
  assert.deepEqual(explicitRuntimeInvocation.args, ['fixture-command.cjs']);

  const wordpressPackageExports = require('..');
  assert.equal(Object.hasOwn(wordpressPackageExports, 'createAuditWpCodeboxFanoutPlan'), false);
  assert.equal(Object.hasOwn(wordpressPackageExports, 'executeAuditWpCodeboxFanout'), false);
  assert.equal(Object.hasOwn(wordpressPackageExports.wpCodebox || {}, 'createAuditWpCodeboxFanoutPlan'), false);
  assert.equal(Object.hasOwn(wordpressPackageExports.wpCodebox || {}, 'executeAuditWpCodeboxFanout'), false);

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
  assert.equal(initialPlan.audit.task_count, 2);
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
  assert.match(phpcsRequest.task.prompt, /remediation group PHPCS Formatting\/Auto Fix!/);
  assert.match(phpcsRequest.task.prompt, /finding-phpcs-001/);
  assert.match(phpcsRequest.task.prompt, /finding-phpcs-002/);
  assert.match(phpcsRequest.task.prompt, /reviewed artifact/);
  assert.match(phpcsRequest.task.prompt, /opens pull requests outside the sandbox/);

  const nestedArtifactOutcome = taskOutcome(phpcsRequest, {
    success: true,
    executions: [
      {
        command: 'wordpress.run-php',
        stdout: JSON.stringify({
          command: 'agent-sandbox.run',
          output: JSON.stringify({
            agent_runtime: {
              success: true,
              result: {
                outcome: {
                  kind: 'fix_artifact',
                  artifact: {
                    id: 'artifact-fixture-nested',
                    directory: '/tmp/artifact-fixture-nested',
                    changed_files: [{ relative_path: 'src/Example.php' }],
                  },
                },
              },
            },
          }),
        }),
      },
    ],
  }, { id: 'artifact-fixture-nested' }, true);
  assert.equal(nestedArtifactOutcome.kind, 'fix_artifact');
  assert.equal(nestedArtifactOutcome.artifact.id, 'artifact-fixture-nested');
  assert.deepEqual(nestedArtifactOutcome.finding_ids, ['finding-phpcs-001', 'finding-phpcs-002']);

  const artifactOnlyBundle = createBundle(
    root,
    'artifact-only-bundle',
    '/wordpress/wp-content/plugins/fixture-plugin/src/ArtifactOnly.php',
    'src/ArtifactOnly.php'
  );
  const artifactOnlyOutcome = taskOutcome(phpcsRequest, {
    success: true,
    artifacts: {
      id: artifactOnlyBundle.artifactId,
      directory: artifactOnlyBundle.bundle,
    },
    executions: [
      {
        stdout: JSON.stringify({ output: 'Agent completed with a reviewed artifact.' }),
      },
    ],
  }, { id: artifactOnlyBundle.artifactId, directory: artifactOnlyBundle.bundle }, true);
  assert.equal(artifactOnlyOutcome.kind, 'fix_artifact');
  assert.equal(artifactOnlyOutcome.artifact.changed_files[0].relative_path, 'src/ArtifactOnly.php');
  const falsePositiveOutcome = taskOutcome(phpcsRequest, {
    outcome: {
      kind: 'false_positive_pr',
      false_positive_pr_url: 'https://github.com/Extra-Chill/homeboy-extensions/pull/779',
    },
  }, null, true);
  assert.equal(falsePositiveOutcome.kind, 'false_positive_pr');
  assert.equal(falsePositiveOutcome.false_positive_pr_url, 'https://github.com/Extra-Chill/homeboy-extensions/pull/779');
  assert.deepEqual(falsePositiveOutcome.finding_ids, ['finding-phpcs-001', 'finding-phpcs-002']);

  const structuredFixOutcome = taskOutcome(phpcsRequest, {
    outcome: {
      kind: 'fix_pr',
      pr_url: 'https://github.com/Extra-Chill/homeboy-extensions/pull/780',
      remediation_summary: 'Fixture fix PR opened.',
    },
  }, { id: 'artifact-fixture-fix' }, true);
  assert.equal(structuredFixOutcome.kind, 'fix_pr');
  assert.equal(structuredFixOutcome.pr_url, 'https://github.com/Extra-Chill/homeboy-extensions/pull/780');
  assert.equal(structuredFixOutcome.artifact_id, 'artifact-fixture-fix');
  assert.equal(structuredFixOutcome.remediation_summary, 'Fixture fix PR opened.');
  assert.deepEqual(structuredFixOutcome.finding_ids, ['finding-phpcs-001', 'finding-phpcs-002']);

  const providerErrorOutcome = taskOutcome(phpcsRequest, {
    outcome: {
      kind: 'provider_error',
      retryable: true,
      provider: 'openai',
      provider_error: {
        code: 'rate_limit_exceeded',
        message: 'Fixture provider returned 429',
        retryable: true,
      },
      metadata: {
        datamachine: {
          completed: false,
          max_turns_reached: false,
        },
      },
    },
  }, null, false, 'WP Codebox task reported success=false');
  assert.equal(providerErrorOutcome.kind, 'provider_error');
  assert.equal(providerErrorOutcome.retryable, true);
  assert.equal(providerErrorOutcome.provider_error.code, 'rate_limit_exceeded');
  assert.equal(providerErrorOutcome.failure, 'Fixture provider returned 429');
  assert.deepEqual(providerErrorOutcome.finding_ids, ['finding-phpcs-001', 'finding-phpcs-002']);
  assert.equal(providerErrorOutcome.metadata.datamachine.completed, false);

  const noPrOutcome = taskOutcome(docsRequest, {
    outcome: {
      kind: 'agent_no_pr_outcome',
      message: 'Agent completed without opening a required PR.',
      metadata: {
        datamachine: {
          completed: true,
          max_turns_reached: false,
        },
      },
    },
  }, null, true);
  assert.equal(noPrOutcome.kind, 'unable_to_remediate');
  assert.equal(noPrOutcome.failure, 'Agent completed without opening a required PR.');
  assert.deepEqual(noPrOutcome.finding_ids, ['finding-doc-001']);
  assert.equal(noPrOutcome.metadata.datamachine.completed, true);

  const zeroChangeOutcome = taskOutcome(docsRequest, {
    success: true,
    artifacts: {
      id: 'artifact-zero-change',
      directory: path.join(root, 'zero-change-artifact'),
    },
  }, { id: 'artifact-zero-change', directory: path.join(root, 'zero-change-artifact') }, true);
  assert.equal(zeroChangeOutcome.kind, 'explicit_failure');
  assert.equal(zeroChangeOutcome.non_actionable, true);
  assert.equal(taskOutcomeSucceeded(zeroChangeOutcome), false);
  assert.deepEqual(zeroChangeOutcome.finding_ids, ['finding-doc-001']);

  const noopOutcome = taskOutcome(docsRequest, {
    outcome: {
      kind: 'noop_artifact',
      false_positive: true,
    },
  }, null, true);
  assert.equal(noopOutcome.kind, 'noop_artifact');
  assert.equal(noopOutcome.false_positive, true);
  assert.equal(taskOutcomeSucceeded(noopOutcome), true);

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
  assert.equal(phpcsApplyBack.change_artifact.id, phpcsBundle.artifactId);
  assert.equal(phpcsApplyBack.change_artifact.type, 'wp_codebox_patch');
  assert.equal(phpcsApplyBack.change_artifact.provenance.source, 'wp-codebox');
  assert.equal(phpcsApplyBack.change_artifact.approval_scope.scope, 'artifact');
  assert.equal(phpcsApplyBack.apply_request.id, `apply-request-${phpcsBundle.artifactId}`);
  assert.equal(phpcsApplyBack.apply_request.artifact.id, phpcsBundle.artifactId);
  assert.equal(phpcsApplyBack.apply_request.approval_scope.scope, 'artifact');
  assert.deepEqual(phpcsApplyBack.apply_request.policy.approved_files, [phpcsBundle.changedPath]);
  assert.equal(phpcsApplyBack.apply_request.policy.content_digest, phpcsBundle.contentDigest);
  assert.equal(phpcsApplyBack.apply_request.policy.patch_sha256, phpcsBundle.patchSha256);
  assert.equal(phpcsApplyBack.apply_request.policy.publish.push, false);
  assert.equal(phpcsApplyBack.apply_request.policy.publish.open_pull_request, false);
  assert.equal(phpcsApplyBack.apply_request.inputs.bundlePath, fs.realpathSync(phpcsBundle.bundle));
  assert.equal(phpcsApplyBack.apply_request.inputs.branch, 'fix/homeboy-audit/phpcs-formatting-auto-fix');
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
    issue_url: 'https://github.com/Extra-Chill/homeboy-extensions/issues/769',
      artifact_map: {
        ...artifactMap,
      'docs-reference': {
        bundle_path: docsBundle.bundle,
        approved_files: [docsBundle.changedPath],
        approved: false,
        false_positive: true,
        reason: 'Fixture docs-reference rule is intentionally noisy.',
      },
    },
  });
  assert.equal(rejectedPlan.apply_back.length, 1);
  assert.equal(rejectedPlan.apply_back[0].group_key, 'PHPCS Formatting/Auto Fix!');
  assert.equal(rejectedPlan.issue_reports.length, 1);
  assert.equal(rejectedPlan.issue_reports[0].schema, 'homeboy/audit-wp-codebox-issue-report/v1');
  assert.equal(rejectedPlan.issue_reports[0].group_key, 'docs-reference');
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
  const execution = await executeAuditWpCodeboxFanout({
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
  assert.equal(completedRecord.finding_id, 'finding-phpcs-001');
  assert.deepEqual(completedRecord.finding_ids, ['finding-phpcs-001', 'finding-phpcs-002']);
  assert.equal(completedRecord.command.bin, process.execPath);
  assert.equal(completedRecord.result.session.id, completedRecord.sandbox_session_id);
  assert.equal(completedRecord.result.session.orchestrator.issue_url, 'https://github.com/Extra-Chill/homeboy-extensions/issues/773');
  assert.match(completedRecord.artifact.id, /^artifact-homeboy-audit-/);
  assert.equal(completedRecord.artifact.runtime_reference_manifest.available, true);
  assert.equal(completedRecord.artifact.runtime_reference_manifest.payload.schema, 'wp-codebox/runtime-reference-manifest-fixture/v1');
  assert.equal(completedRecord.artifact.runtime_reference_manifest.payload.cookie, '[redacted]');
  assert.equal(completedRecord.outcome.kind, 'fix_pr');
  assert.equal(completedRecord.outcome.pr_url, 'https://github.com/Extra-Chill/homeboy-extensions/pull/777');
  assertMetrics(completedRecord, { runnerMetrics: true, artifactBytes: true });
  assert.equal(execution.outcomes.length, 2);
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
  const nestedFailedRecord = nestedErrorExecution.records.find((record) => record.group_key === 'docs-reference');
  assert.equal(nestedErrorExecution.status, 'failed');
  assert.equal(nestedFailedRecord.status, 'failed');
  assert.equal(nestedFailedRecord.command.exit_code, 0);
  assert.match(nestedFailedRecord.command.error, /Fixture nested WP error/);

  const providerRunsOutputPath = path.join(root, 'fanout-provider-error-run.json');
  const providerErrorExecution = await executeAuditWpCodeboxFanout({
    report,
    wp_codebox_command: process.execPath,
    wp_codebox_args: [fixtureCommand],
    runsOutputPath: providerRunsOutputPath,
    env: { FIXTURE_PROVIDER_ERROR_GROUP: 'PHPCS Formatting/Auto Fix!' },
  });
  const providerFailedRecord = providerErrorExecution.records.find((record) => record.group_key === 'PHPCS Formatting/Auto Fix!');
  assert.equal(providerErrorExecution.status, 'failed');
  assert.equal(providerFailedRecord.status, 'failed');
  assert.equal(providerFailedRecord.outcome.kind, 'provider_error');
  assert.equal(providerFailedRecord.outcome.retryable, true);
  assert.equal(providerFailedRecord.outcome.provider_error.message, 'Fixture provider returned 429');
  assert.deepEqual(providerFailedRecord.outcome.finding_ids, ['finding-phpcs-001', 'finding-phpcs-002']);
  const providerFinalRun = readJson(providerRunsOutputPath);
  assert.equal(providerFinalRun.outcomes.find((outcome) => outcome.kind === 'provider_error').retryable, true);

  const noopExecution = await executeAuditWpCodeboxFanout({
    report,
    wp_codebox_command: process.execPath,
    wp_codebox_args: [fixtureCommand],
    concurrency: 1,
    env: { FIXTURE_NOOP_GROUP: 'PHPCS Formatting/Auto Fix!' },
  });
  const noopRecord = noopExecution.records.find((record) => record.group_key === 'PHPCS Formatting/Auto Fix!');
  assert.equal(noopRecord.status, 'completed');
  assert.equal(noopRecord.outcome.kind, 'noop_artifact');
  assertMetrics(noopRecord, { runnerMetrics: true });
  assert.equal(noopRecord.metrics.artifact_bytes, null);

  const noActionableExecution = await executeAuditWpCodeboxFanout({
    report,
    wp_codebox_command: process.execPath,
    wp_codebox_args: [fixtureCommand],
    concurrency: 1,
    env: { FIXTURE_NO_ACTIONABLE_GROUP: 'PHPCS Formatting/Auto Fix!' },
  });
  const noActionableRecord = noActionableExecution.records.find((record) => record.group_key === 'PHPCS Formatting/Auto Fix!');
  assert.equal(noActionableExecution.status, 'failed');
  assert.equal(noActionableRecord.status, 'failed');
  assert.equal(noActionableRecord.command.exit_code, 0);
  assert.equal(noActionableRecord.outcome.kind, 'explicit_failure');
  assert.equal(noActionableRecord.outcome.non_actionable, true);
  assert.equal(noActionableRecord.outcome.evidence.changed_file_count, 0);
  assert.match(noActionableRecord.outcome.failure, /without an actionable patch/);
  assert.equal(noActionableRecord.outcome.failure_metadata.exit_code, 0);
  assert.equal(noActionableRecord.outcome.failure_metadata.group_key, 'PHPCS Formatting/Auto Fix!');
  assertMetrics(noActionableRecord, { runnerMetrics: true, artifactBytes: true });

  const timeoutRunsOutputPath = path.join(root, 'fanout-timeout-run.json');
  const timeoutArtifactsRoot = path.join(root, 'timeout-artifacts');
  const timeoutExecution = await executeAuditWpCodeboxFanout({
    report,
    wp_codebox_command: process.execPath,
    wp_codebox_args: [fixtureCommand, '--artifacts', timeoutArtifactsRoot],
    concurrency: 1,
    task_timeout_seconds: 1,
    runsOutputPath: timeoutRunsOutputPath,
    env: { FIXTURE_HANG_GROUP: 'PHPCS Formatting/Auto Fix!' },
  });
  assert.equal(timeoutExecution.status, 'failed');
  assert.equal(timeoutExecution.records.length, 2);
  const timeoutRecord = timeoutExecution.records.find((record) => record.group_key === 'PHPCS Formatting/Auto Fix!');
  assert.equal(timeoutRecord.status, 'failed');
  assert.equal(timeoutRecord.command.timed_out, true);
  assert.equal(timeoutRecord.command.timeout_seconds, 1);
  assert.equal(timeoutRecord.command.killed_process_group, true);
  assert.match(timeoutRecord.command.error, /timed out after 1 seconds/);
  assert.equal(timeoutRecord.outcome.kind, 'timeout');
  assert.equal(timeoutRecord.outcome.failure_metadata.group_key, 'PHPCS Formatting/Auto Fix!');
  assert.equal(timeoutRecord.outcome.failure_metadata.sandbox_session_id, timeoutRecord.sandbox_session_id);
  assert.equal(timeoutRecord.outcome.failure_metadata.timed_out, true);
  assert.equal(timeoutRecord.outcome.failure_metadata.timeout_seconds, 1);
  assert.equal(timeoutRecord.outcome.failure_metadata.partial_artifact_count, 1);
  assert.equal(timeoutRecord.partial_artifacts.length, 1);
  assert.equal(timeoutRecord.partial_artifacts[0].has_changed_files, true);
  assert.equal(timeoutRecord.partial_artifacts[0].runtime_reference_manifest.available, true);
  assert.equal(timeoutRecord.partial_artifacts[0].runtime_reference_manifest.payload.cookie, '[redacted]');
  assert.equal(timeoutRecord.outcome.partial_artifacts[0].directory, timeoutRecord.partial_artifacts[0].directory);
  assert.match(timeoutRecord.stdout, /fixture partial stdout before timeout/);
  assert.match(timeoutRecord.stderr, /fixture partial stderr before timeout/);
  assertMetrics(timeoutRecord);
  assert.equal(timeoutRecord.metrics.sample_count, 0);
  assert.equal(timeoutRecord.metrics.artifact_bytes, null);

  const coreModulePath = path.join(root, 'wp-codebox-core-partial-discovery-fixture.mjs');
  fs.writeFileSync(coreModulePath, [
    'export async function discoverPartialRunArtifacts(options) {',
    '  return {',
    '    schema: "wp-codebox/partial-artifact-discovery/v1",',
    '    artifactsRoot: options.artifactsRoot,',
    '    sessionId: options.sessionId,',
    '    selectedBy: "session-id",',
    '    candidateCount: 1,',
    '    artifacts: [{',
    '      directory: `${options.artifactsRoot}/core-${options.sessionId}`,',
    '      bytes: 321,',
    '      mtime: "2026-06-06T12:00:01.000Z",',
    '      hasManifest: false,',
    '      hasChangedFiles: true,',
    '      hasRuntimeReferenceManifest: true,',
    '      manifest: { path: `${options.artifactsRoot}/core/manifest.json`, relativePath: "manifest.json", available: false },',
    '      changedFiles: { path: `${options.artifactsRoot}/core/files/changed-files.json`, relativePath: "files/changed-files.json", available: true },',
    '      runtimeReferenceManifest: { path: `${options.artifactsRoot}/core/files/runtime-reference-manifest.json`, relativePath: "files/runtime-reference-manifest.json", available: true, payload: { schema: "wp-codebox/runtime-reference-manifest-fixture/v1", token: "[redacted]" } }',
    '    }]',
    '  };',
    '}',
    '',
  ].join('\n'));
  const coreDiscoveryExecution = await executeAuditWpCodeboxFanout({
    report,
    wp_codebox_command: process.execPath,
    wp_codebox_args: [fixtureCommand, '--artifacts', path.join(root, 'core-discovery-artifacts')],
    concurrency: 1,
    task_timeout_seconds: 1,
    wpCodeboxCoreModule: coreModulePath,
    env: { FIXTURE_HANG_GROUP: 'PHPCS Formatting/Auto Fix!' },
  });
  const coreDiscoveryRecord = coreDiscoveryExecution.records.find((record) => record.group_key === 'PHPCS Formatting/Auto Fix!');
  assert.equal(coreDiscoveryRecord.partial_artifacts.length, 1);
  assert.match(coreDiscoveryRecord.partial_artifacts[0].directory, /core-homeboy-audit-/);
  assert.equal(coreDiscoveryRecord.partial_artifacts[0].has_changed_files, true);
  assert.equal(coreDiscoveryRecord.partial_artifacts[0].changed_files_path.endsWith('files/changed-files.json'), true);
  assert.equal(coreDiscoveryRecord.partial_artifacts[0].runtime_reference_manifest.payload.token, '[redacted]');

  const timeoutFollowupRecord = timeoutExecution.records.find((record) => record.group_key === 'docs-reference');
  assert.equal(timeoutFollowupRecord.status, 'failed');
  assert.equal(timeoutFollowupRecord.command.exit_code, 3);
  assert.equal(timeoutFollowupRecord.outcome.failure, 'WP Codebox task exited with code 3');
  assert.equal(timeoutFollowupRecord.outcome.failure_metadata.exit_code, 3);
  assert.equal(timeoutFollowupRecord.outcome.failure_metadata.group_key, 'docs-reference');
  const timeoutFinalRun = readJson(timeoutRunsOutputPath);
  assert.equal(timeoutFinalRun.status, 'failed');
  assert.equal(timeoutFinalRun.records.length, 2);
  assert.equal(timeoutFinalRun.outcomes.length, 2);
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
  assert.equal(fileExecution.records.length, 2);
  const finalRun = readJson(runsOutputPath);
  assert.equal(finalRun.records.length, 2);
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
  assert.equal(cliExecution.records.length, 2);
  assert.equal(cliExecution.records[0].schema, 'homeboy/audit-wp-codebox-run/v1');
  assert.match(cliExecutionResult.stderr, /\[homeboy wp-codebox fanout\] started 1\/2 group=PHPCS Formatting\/Auto Fix! session=homeboy-audit-/);
  assert.match(cliExecutionResult.stderr, /\[homeboy wp-codebox fanout\] completed 1\/2 group=PHPCS Formatting\/Auto Fix! session=homeboy-audit-.*artifact=.*artifact-homeboy-audit-/);
  assert.match(cliExecutionResult.stderr, /\[homeboy wp-codebox fanout\] failed 2\/2 group=docs-reference session=homeboy-audit-/);
  assert.doesNotMatch(cliExecutionResult.stderr, /FIXTURE_SECRET_TOKEN/);

  const cliProviderExecutionResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-audit-wp-codebox-fanout.cjs'),
    '--audit-report',
    auditReportPath,
    '--execute',
    '--wp-codebox-command',
    process.execPath,
    '--wp-codebox-arg',
    fixtureCommand,
  ], {
    encoding: 'utf8',
    env: {
      ...process.env,
      FIXTURE_PROVIDER_ERROR_GROUP: 'PHPCS Formatting/Auto Fix!',
    },
  });
  assert.equal(cliProviderExecutionResult.status, 0, cliProviderExecutionResult.stderr || cliProviderExecutionResult.stdout);
  assert.match(cliProviderExecutionResult.stderr, /failed 1\/2 group=PHPCS Formatting\/Auto Fix! .* outcome=provider_error retryable=yes failure=Fixture provider returned 429/);

  console.log('Homeboy audit WP Codebox fanout smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
