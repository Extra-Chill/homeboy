'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-refactor-source-wp-codebox-'));
const fixtureCodeboxCoreModule = path.join(__dirname, '..', '..', 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');

function pathInside(parent, candidate) {
  const relative = path.relative(fs.realpathSync(parent), path.resolve(candidate));
  return relative === '' || (Boolean(relative) && !relative.startsWith('..') && !path.isAbsolute(relative));
}

try {
  const auditResult = JSON.parse(fs.readFileSync(
    path.join(__dirname, 'fixtures', 'homeboy-audit-wp-codebox-fanout', 'audit-report.json'),
    'utf8'
  ));
  const outputDir = path.join(root, 'fanout');
  const command = {
    command: 'refactor_source',
    source: 'audit',
    component_id: 'fixture-plugin',
    root: '/repo/fixture-plugin',
    source_result: auditResult,
    write: false,
    settings: {
      wp_codebox_output_dir: outputDir,
      wp_codebox_provider: 'opencode',
      wp_codebox_model: 'opencode-go/kimi-k2.6',
      wp_codebox_provider_plugin_paths: '/plugins/ai-provider-for-opencode',
      wp_codebox_secret_env: 'OPENCODE_API_KEY',
      wp_codebox_issue_url: 'https://github.com/Extra-Chill/homeboy-extensions/issues/769',
    },
  };

  const result = spawnSync('python3', [path.join(__dirname, '..', 'scripts', 'refactor.py')], {
    encoding: 'utf8',
    input: JSON.stringify(command),
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  const response = JSON.parse(result.stdout);
  assert.equal(response.handled, true);
  assert.equal(response.detected_findings, 3);
  assert.equal(response.changed_files.length, 0);
  assert.equal(response.fix_results.length, 2);
  assert.equal(response.fix_results[0].rule, 'wp_codebox.audit_fanout');
  assert.equal(response.fix_results[0].primitive, 'extension_refactor_source');
  assert.match(response.warnings[0], /fanout-plan\.json/);

  const plan = JSON.parse(fs.readFileSync(path.join(outputDir, 'fanout-plan.json'), 'utf8'));
  assert.equal(plan.schema, 'homeboy/audit-wp-codebox-fanout/v1');
  assert.equal(plan.task_requests.length, 2);
  assert.equal(plan.task_requests[0].provider, 'opencode');
  assert.equal(plan.task_requests[0].model, 'opencode-go/kimi-k2.6');
  assert.deepEqual(plan.task_requests[0].provider_plugin_paths, ['/plugins/ai-provider-for-opencode']);
  assert.deepEqual(plan.task_requests[0].secret_env, ['OPENCODE_API_KEY']);

  const fixtureWpCli = path.join(root, 'fixture-wp-cli.cjs');
  fs.writeFileSync(fixtureWpCli, `#!/usr/bin/env node
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const inputArg = process.argv.find((arg) => arg.startsWith('--input-file='));
const inputPath = inputArg ? inputArg.slice('--input-file='.length) : '';
if (!['agent-task-run', 'run-agent-task'].includes(process.argv[2]) || !inputPath) {
  process.exit(2);
}
const runRequest = JSON.parse(fs.readFileSync(inputPath, 'utf8'));
const task = runRequest.task_input || runRequest;
const artifactsRoot = task.artifacts_path || '';
const sessionId = task.sandbox_session_id;
if (process.env.FIXTURE_HANG_GROUP === task.group_key) {
  if (artifactsRoot) {
    const partialDirectory = path.join(artifactsRoot, 'partial-' + sessionId);
    const filesDirectory = path.join(partialDirectory, 'files');
    fs.mkdirSync(filesDirectory, { recursive: true });
    fs.writeFileSync(path.join(filesDirectory, 'changed-files.json'), JSON.stringify({
      schema: 'wp-codebox/changed-files/v1',
      files: [
        {
          path: '/workspace/homeboy-extensions/wordpress/docs/partial.md',
          status: 'modified',
          mountTarget: '/workspace/homeboy-extensions',
          relativePath: 'wordpress/docs/partial.md'
        }
      ]
    }, null, 2) + '\\n');
  }
  process.stderr.write('fixture task hung before producing an artifact\\n');
  setInterval(() => {}, 1000);
  return;
}
const artifactDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'fixture-wp-codebox-artifact-'));
if (task.group_key === 'PHPCS Formatting/Auto Fix!') {
  const filesDirectory = path.join(artifactDirectory, 'files');
  fs.mkdirSync(filesDirectory, { recursive: true });
  fs.writeFileSync(path.join(filesDirectory, 'changed-files.json'), JSON.stringify({
    schema: 'wp-codebox/changed-files/v1',
    files: [
      {
        path: '/workspace/agents-api/src/example.php',
        status: 'modified',
        mountTarget: '/workspace/agents-api',
        relativePath: 'src/example.php'
      }
    ]
  }, null, 2) + '\\n');
  if (process.env.FIXTURE_INVALID_PATCH) {
    fs.writeFileSync(path.join(filesDirectory, 'patch.diff'), [
      'diff --git /dev/null b/workspace/agents-api/src/example.php',
      '--- /dev/null',
      '+++ b/workspace/agents-api/src/example.php',
      '@@ -0,0 +1 @@',
      '+after',
      ''
    ].join('\\n'));
  } else {
    fs.writeFileSync(path.join(filesDirectory, 'patch.diff'), [
      'diff --git a/workspace/agents-api/src/example.php b/workspace/agents-api/src/example.php',
      '--- a/workspace/agents-api/src/example.php',
      '+++ b/workspace/agents-api/src/example.php',
      '@@ -1 +1 @@',
      '-before',
      '+after',
      ''
    ].join('\\n'));
  }
} else if (task.group_key === 'docs-reference') {
  const filesDirectory = path.join(artifactDirectory, 'files');
  fs.mkdirSync(filesDirectory, { recursive: true });
  fs.writeFileSync(path.join(filesDirectory, 'changed-files.json'), JSON.stringify({
    schema: 'wp-codebox/changed-files/v1',
    files: [
      {
        path: '/workspace/homeboy-extensions/wordpress/docs/example.md',
        status: 'modified',
        mountTarget: '/workspace/homeboy-extensions',
        relativePath: 'wordpress/docs/example.md'
      }
    ]
  }, null, 2) + '\\n');
  fs.writeFileSync(path.join(filesDirectory, 'patch.diff'), [
    'diff --git a/workspace/homeboy-extensions/wordpress/docs/example.md b/workspace/homeboy-extensions/wordpress/docs/example.md',
    '--- a/workspace/homeboy-extensions/wordpress/docs/example.md',
    '+++ b/workspace/homeboy-extensions/wordpress/docs/example.md',
    '@@ -1 +1 @@',
    '-stale',
    '+fresh',
    ''
  ].join('\\n'));
}
process.stdout.write(JSON.stringify({
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  session: { id: sessionId },
  artifacts: {
    id: 'artifact-' + sessionId,
    directory: artifactDirectory,
    path: artifactDirectory
  },
  agent_result: { status: 'completed' },
  outcome: {
    pr_url: 'https://github.com/Extra-Chill/homeboy-extensions/pull/' + (task.group_key === 'PHPCS Formatting/Auto Fix!' ? '777' : '778')
  }
}));
`);
  fs.chmodSync(fixtureWpCli, 0o755);

  const writeOutputDir = path.join(root, 'fanout-write');
  const writeCommand = {
    ...command,
    root: path.join(root, 'agents-api'),
    write: true,
    settings: {
      ...command.settings,
      wp_codebox_output_dir: writeOutputDir,
      wp_codebox_bin: fixtureWpCli,
      wp_codebox_agents_api_path: path.join(root, 'agents-api'),
      wp_codebox_runtime_path: path.join(root, 'example-runtime'),
      wp_codebox_runtime_tools_path: path.join(root, 'example-runtime-tools'),
      wp_codebox_homeboy_path: path.join(root, 'homeboy'),
      wp_codebox_homeboy_extensions_path: path.join(root, 'homeboy-extensions'),
      wp_codebox_task_timeout_seconds: 2,
    },
  };
  fs.mkdirSync(writeCommand.settings.wp_codebox_agents_api_path, { recursive: true });
  fs.mkdirSync(writeCommand.settings.wp_codebox_runtime_path, { recursive: true });
  fs.mkdirSync(writeCommand.settings.wp_codebox_runtime_tools_path, { recursive: true });
  fs.mkdirSync(writeCommand.settings.wp_codebox_homeboy_path, { recursive: true });
  fs.mkdirSync(writeCommand.settings.wp_codebox_homeboy_extensions_path, { recursive: true });
  fs.mkdirSync(path.join(writeCommand.settings.wp_codebox_agents_api_path, 'src'), { recursive: true });
  fs.mkdirSync(path.join(writeCommand.settings.wp_codebox_homeboy_extensions_path, 'wordpress', 'docs'), { recursive: true });
  fs.writeFileSync(path.join(writeCommand.settings.wp_codebox_agents_api_path, 'src', 'example.php'), 'before\n');
  fs.writeFileSync(path.join(writeCommand.settings.wp_codebox_homeboy_extensions_path, 'wordpress', 'docs', 'example.md'), 'stale\n');

  const writeResult = spawnSync('python3', [path.join(__dirname, '..', 'scripts', 'refactor.py')], {
    encoding: 'utf8',
    input: JSON.stringify(writeCommand),
    env: {
      ...process.env,
      HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCodeboxCoreModule,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });

  assert.equal(writeResult.status, 0, writeResult.stderr || writeResult.stdout);
  const writeResponse = JSON.parse(writeResult.stdout);
  assert.equal(writeResponse.handled, true);
  assert.deepEqual(writeResponse.changed_files, ['agents-api:src/example.php', 'homeboy-extensions:wordpress/docs/example.md']);
  assert.equal(fs.readFileSync(path.join(writeCommand.settings.wp_codebox_agents_api_path, 'src', 'example.php'), 'utf8'), 'after\n');
  assert.equal(fs.readFileSync(path.join(writeCommand.settings.wp_codebox_homeboy_extensions_path, 'wordpress', 'docs', 'example.md'), 'utf8'), 'fresh\n');
  assert.match(writeResult.stderr, /\[homeboy wp-codebox fanout\] started 1\/2 group=PHPCS Formatting\/Auto Fix! session=homeboy-audit-/);
  assert.match(writeResult.stderr, /\[homeboy wp-codebox fanout\] completed 2\/2 group=docs-reference session=homeboy-audit-.*artifact=.*fixture-wp-codebox-artifact-/);
  assert.doesNotMatch(writeResult.stderr, /OPENCODE_API_KEY/);
  assert.match(writeResponse.warnings[1], /fanout-run\.json/);
  const run = JSON.parse(fs.readFileSync(path.join(writeOutputDir, 'fanout-run.json'), 'utf8'));
  assert.equal(run.status, 'completed');
  assert.equal(run.records[0].command.bin, 'node');
  assert.equal(run.records[0].command.timeout_seconds, 2);
  assert.match(run.records[0].command.args[0], /homeboy-wp-codebox-task-runner\.cjs$/);
  assert.equal(run.records[0].command.args.includes('--wp-codebox-bin'), true);
  assert.equal(run.records[0].command.args.includes('--task-timeout-seconds'), true);
  assert.equal(run.records[0].command.args.includes('--homeboy'), true);
  assert.equal(run.records[0].command.args.includes('--homeboy-extensions'), true);
  assert.equal(run.records[0].command.args.includes('--agents-api'), false);
  assert.equal(run.records[0].command.args.includes('--data-machine'), false);
  assert.equal(run.records[0].command.args.includes('--data-machine-code'), false);
  const artifactsIndex = run.records[0].command.args.indexOf('--artifacts');
  assert.notEqual(artifactsIndex, -1);
  assert.equal(pathInside(writeCommand.root, run.records[0].command.args[artifactsIndex + 1]), false);
  assert.equal(run.records[0].artifact.id.startsWith('artifact-homeboy-audit-'), true);

  const invalidPatchResult = spawnSync('python3', [path.join(__dirname, '..', 'scripts', 'refactor.py')], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...writeCommand,
      settings: {
        ...writeCommand.settings,
        wp_codebox_output_dir: path.join(root, 'fanout-invalid-patch'),
      },
    }),
    env: {
      ...process.env,
      HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCodeboxCoreModule,
      FIXTURE_INVALID_PATCH: '1',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.notEqual(invalidPatchResult.status, 0);
  assert.match(invalidPatchResult.stderr, /WP Codebox artifact apply failed for group PHPCS Formatting\/Auto Fix! artifact artifact-homeboy-audit-/);
  assert.match(invalidPatchResult.stderr, /artifact directory: .*fixture-wp-codebox-artifact-/);
  assert.match(invalidPatchResult.stderr, /patch path: .*files\/patch-agents-api\.diff/);
  assert.match(invalidPatchResult.stderr, /fanout run evidence: .*fanout-invalid-patch\/fanout-run\.json/);
  assert.match(invalidPatchResult.stderr, /git diff header lacks filename information|patch with only garbage/);
  assert.ok(fs.existsSync(path.join(root, 'fanout-invalid-patch', 'fanout-run.json')));

  fs.writeFileSync(path.join(writeCommand.settings.wp_codebox_agents_api_path, 'src', 'example.php'), 'before\n');
  fs.writeFileSync(path.join(writeCommand.settings.wp_codebox_homeboy_extensions_path, 'wordpress', 'docs', 'example.md'), 'stale\n');
  const partialFailureResult = spawnSync('python3', [path.join(__dirname, '..', 'scripts', 'refactor.py')], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...writeCommand,
      settings: {
        ...writeCommand.settings,
        wp_codebox_output_dir: path.join(root, 'fanout-partial-failure'),
        wp_codebox_task_timeout_seconds: 1,
      },
    }),
    env: {
      ...process.env,
      HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCodeboxCoreModule,
      FIXTURE_HANG_GROUP: 'docs-reference',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(partialFailureResult.status, 0, partialFailureResult.stderr || partialFailureResult.stdout);
  const partialFailureResponse = JSON.parse(partialFailureResult.stdout);
  assert.deepEqual(partialFailureResponse.changed_files, ['agents-api:src/example.php']);
  assert.equal(fs.readFileSync(path.join(writeCommand.settings.wp_codebox_agents_api_path, 'src', 'example.php'), 'utf8'), 'after\n');
  assert.equal(fs.readFileSync(path.join(writeCommand.settings.wp_codebox_homeboy_extensions_path, 'wordpress', 'docs', 'example.md'), 'utf8'), 'stale\n');
  assert.match(partialFailureResponse.warnings.join('\n'), /partial failure/);
  assert.match(partialFailureResponse.warnings.join('\n'), /WP Codebox task timed out after 1 seconds/);
  const partialRun = JSON.parse(fs.readFileSync(path.join(root, 'fanout-partial-failure', 'fanout-run.json'), 'utf8'));
  assert.equal(partialRun.status, 'failed');
  const partialFailedRecord = partialRun.records.find((record) => record.group_key === 'docs-reference');
  assert.equal(partialFailedRecord.outcome.failure_metadata.group_key, 'docs-reference');
  assert.equal(partialFailedRecord.outcome.failure_metadata.timed_out, true);
  assert.equal(partialFailedRecord.partial_artifacts.length, 1);
  assert.equal(partialFailedRecord.partial_artifacts[0].has_changed_files, true);

  const missingSecretResult = spawnSync('python3', [path.join(__dirname, '..', 'scripts', 'refactor.py')], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...writeCommand,
      settings: {
        ...writeCommand.settings,
        wp_codebox_output_dir: path.join(root, 'fanout-missing-secret'),
      },
    }),
    env: {
      ...process.env,
      HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCodeboxCoreModule,
      OPENCODE_API_KEY: '',
    },
  });
  assert.notEqual(missingSecretResult.status, 0);
  assert.match(missingSecretResult.stderr, /WP Codebox audit fan-out failed: 2 of 2 task\(s\) failed/);
  assert.match(missingSecretResult.stderr, /Required WP Codebox secret environment variable missing: OPENCODE_API_KEY/);

  const riskyRoot = path.join(root, 'risky-source');
  const riskyCommand = {
    ...command,
    root: riskyRoot,
    settings: {
      ...command.settings,
      wp_codebox_output_dir: path.join(riskyRoot, 'fanout'),
      wp_codebox_artifacts: path.join(riskyRoot, 'artifacts'),
    },
  };
  fs.mkdirSync(riskyRoot, { recursive: true });
  const riskyResult = spawnSync('python3', [path.join(__dirname, '..', 'scripts', 'refactor.py')], {
    encoding: 'utf8',
    input: JSON.stringify(riskyCommand),
  });
  assert.equal(riskyResult.status, 0, riskyResult.stderr || riskyResult.stdout);
  const riskyResponse = JSON.parse(riskyResult.stdout);
  assert.match(riskyResponse.warnings.join('\n'), /output directory is inside the source tree/);
  assert.match(riskyResponse.warnings.join('\n'), /artifact directory is inside the source tree/);

  console.log('WordPress refactor source WP Codebox smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
