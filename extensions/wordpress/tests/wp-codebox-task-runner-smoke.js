'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function pathInside(parent, candidate) {
  const relative = path.relative(fs.realpathSync(parent), path.resolve(candidate));
  return relative === '' || (Boolean(relative) && !relative.startsWith('..') && !path.isAbsolute(relative));
}

function createFixtureWpCli(root, mode = 0o755) {
  const binPath = path.join(root, 'fixture-wp-cli.js');
  fs.writeFileSync(binPath, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const out = process.env.FIXTURE_WP_CLI_CAPTURE;
const inputIndex = process.argv.indexOf('--input-file');
const inputPath = inputIndex >= 0 ? process.argv[inputIndex + 1] : '';
const input = inputPath ? JSON.parse(fs.readFileSync(inputPath, 'utf8')) : null;
fs.writeFileSync(out, JSON.stringify({ argv: process.argv.slice(2), input }, null, 2));
process.stdout.write(JSON.stringify({
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  session: {
    schema: 'wp-codebox/sandbox-session/v1',
    id: input.sandbox_session_id,
    status: 'completed',
    artifacts: {
      bundle_id: 'artifact-bundle-sha256-fixture',
      preview_url: 'https://preview.example.test/' + input.sandbox_session_id,
    },
    orchestrator: input.orchestrator,
  },
  task: input.parent_request.task.prompt,
  task_input: {
    schema: 'wp-codebox/task-input/v1',
    version: 1,
    goal: input.parent_request.task.prompt,
    target: {},
    allowed_tools: [],
    expected_artifacts: input.parent_request.task.expected_artifacts || [],
    policy: input.parent_request.task.policy || {},
    context: input.parent_request.task.context || {},
  },
  artifacts: input.artifacts_path,
  exit_code: 0,
  run: { agentResult: { status: 'completed' } },
}));
`);
  fs.chmodSync(binPath, mode);
  return binPath;
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-task-runner-'));

try {
  const capturePath = path.join(root, 'capture.json');
  const fixtureWpCli = createFixtureWpCli(root);
  const providerPluginPath = path.join(root, 'example-provider@feature-branch');
  fs.mkdirSync(providerPluginPath, { recursive: true });
  const request = {
    schema: 'homeboy/wp-codebox-task-request/v1',
    sandbox_session_id: 'homeboy-audit-fixture-session',
    group_key: 'PHPCS Formatting/Auto Fix!',
    provider: 'opencode',
    model: 'opencode-go/kimi-k2.6',
    provider_plugin_paths: [providerPluginPath],
    runtime_stack_mounts: [{
      type: 'directory',
      source: '/components/php-ai-client',
      target: '/wordpress/wp-includes/php-ai-client',
      mode: 'readonly',
      metadata: { component: 'php-ai-client', ref: 'custom-provider-auth' },
    }],
    runtime_overlays: [{
      type: 'bundled-library',
      library: 'php-ai-client',
      source: '/components/php-ai-client',
      target: '/wordpress/wp-includes/php-ai-client',
      metadata: { component: 'php-ai-client', ref: 'custom-provider-auth' },
    }],
    secret_env: ['OPENCODE_API_KEY'],
    orchestrator: {
      id: 'homeboy-extensions/audit-wp-codebox-fanout',
      run_id: 'run-123',
      report_id: 'report-123',
      issue_url: 'https://github.com/Extra-Chill/homeboy-extensions/issues/775',
      agent_task_id: 'agent-task-123',
    },
    audit_findings: [
      {
        id: 'finding-1',
        kind: 'wordpress.phpcs.fixable',
        file: 'src/Example.php',
        line: 10,
        message: 'Fix spacing.',
        severity: 'warning',
      },
    ],
    task: {
      title: 'Fix Homeboy audit batch PHPCS Formatting/Auto Fix!',
      prompt: 'Fix the finding.',
      expected_artifacts: ['patch'],
      policy: { kind: 'audit-remediation' },
      context: { source: 'homeboy-smoke' },
    },
  };

  const result = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-cli-bin',
    fixtureWpCli,
    '--wp-codebox-bin',
    '/bin/wp-codebox',
    '--agents-api',
    '/components/agents-api',
    '--data-machine',
    '/components/data-machine',
    '--data-machine-code',
    '/components/data-machine-code',
    '--homeboy',
    '/components/homeboy',
    '--homeboy-extensions',
    '/components/homeboy-extensions',
    '--mount',
    '/repo/plugin:/wordpress/wp-content/plugins/plugin:readwrite',
    '--runtime-stack-mount',
    '/components/wordpress-develop:/wordpress:readonly',
    '--runtime-overlay-json',
    JSON.stringify({
      type: 'wordpress-scoped-bundle',
      source: '/components/wordpress-scoped-bundle',
      scope: 'runtime',
    }),
    '--max-turns',
    '80',
    '--task-timeout-seconds',
    '7200',
    '--artifacts',
    path.join(root, 'artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: {
      ...process.env,
      FIXTURE_WP_CLI_CAPTURE: capturePath,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  const output = JSON.parse(result.stdout);
  assert.equal(output.schema, 'wp-codebox/agent-task-run/v1');
  assert.equal(output.success, true);

  const captured = readJson(capturePath);
  assert.deepEqual(captured.argv.slice(0, 4), ['codebox', 'run-agent-task', '--input-file', captured.argv[3]]);
  assert.equal(captured.argv.includes('--format=json'), true);

  const input = captured.input;
  assert.equal(input.parent_request.schema, 'homeboy/wp-codebox-task-request/v1');
  assert.equal(input.parent_request.orchestrator.issue_url, 'https://github.com/Extra-Chill/homeboy-extensions/issues/775');
  assert.equal(input.parent_request.audit_findings[0].id, 'finding-1');
  assert.equal(input.provider, 'opencode');
  assert.equal(input.model, 'opencode-go/kimi-k2.6');
  assert.equal(input.max_turns, 80);
  assert.equal(input.task_timeout_seconds, 7200);
  assert.equal(input.sandbox_session_id, 'homeboy-audit-fixture-session');
  assert.equal(input.artifacts_path, path.join(root, 'artifacts'));
  assert.equal(input.wp_codebox_bin, '/bin/wp-codebox');
  assert.equal(input.agents_api_path, '/components/agents-api');
  assert.equal(input.data_machine_path, '/components/data-machine');
  assert.equal(input.data_machine_code_path, '/components/data-machine-code');
  assert.deepEqual(input.secret_env, ['OPENCODE_API_KEY']);
  assert.equal(input.mounts[0].source, '/repo/plugin');
  assert.equal(input.runtime_stack_mounts[0].source, '/components/php-ai-client');
  assert.equal(input.runtime_stack_mounts[1].source, '/components/wordpress-develop');
  assert.equal(input.runtime_stack_mounts[1].metadata.kind, 'homeboy-runtime-stack');
  assert.equal(input.runtime_overlays[0].type, 'bundled-library');
  assert.equal(input.runtime_overlays[1].type, 'wordpress-scoped-bundle');
  assert.equal(input.provider_plugin_paths[0], providerPluginPath);
  assert(!JSON.stringify(input).includes('redacted-test-key'));

  const codexCapturePath = path.join(root, 'capture-codex.json');
  const codexSecretEnv = [
    'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
    'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
    'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
  ];
  const codexResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-cli-bin',
    fixtureWpCli,
    '--agents-api',
    '/components/agents-api',
    '--data-machine',
    '/components/data-machine',
    '--data-machine-code',
    '/components/data-machine-code',
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      provider: 'codex',
      model: 'gpt-5.5',
      provider_plugin_paths: ['/components/ai-provider-for-openai'],
      secret_env: codexSecretEnv,
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CLI_CAPTURE: codexCapturePath,
      AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'access-token-value',
      AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'refresh-token-value',
      AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '4102444800',
      AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'account-id-value',
      AI_PROVIDER_OPENAI_CODEX_FEDRAMP: '0',
    },
  });
  assert.equal(codexResult.status, 0, codexResult.stderr || codexResult.stdout);
  const codexInput = readJson(codexCapturePath).input;
  assert.deepEqual(codexInput.secret_env, codexSecretEnv);
  assert.equal(codexInput.provider, 'codex');
  assert.equal(codexInput.model, 'gpt-5.5');
  assert.equal(codexInput.provider_plugin_paths[0], '/components/ai-provider-for-openai');
  const serializedCodexInput = JSON.stringify(codexInput);
  assert(!serializedCodexInput.includes('access-token-value'));
  assert(!serializedCodexInput.includes('refresh-token-value'));
  assert(!serializedCodexInput.includes('wp-ai-gateway'));

  const nonExecutableCapturePath = path.join(root, 'capture-non-executable.json');
  const nonExecutableFixture = createFixtureWpCli(root, 0o644);
  const nonExecutableResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-cli-bin',
    nonExecutableFixture,
    '--agents-api',
    '/components/agents-api',
    '--data-machine',
    '/components/data-machine',
    '--data-machine-code',
    '/components/data-machine-code',
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: {
      ...process.env,
      FIXTURE_WP_CLI_CAPTURE: nonExecutableCapturePath,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(nonExecutableResult.status, 0, nonExecutableResult.stderr || nonExecutableResult.stdout);
  const nonExecutableCapture = readJson(nonExecutableCapturePath);
  assert.equal(nonExecutableCapture.argv[0], 'codebox');
  assert.equal(pathInside(root, nonExecutableCapture.input.artifacts_path), false);

  const requestConfiguredCapturePath = path.join(root, 'capture-request-configured.json');
  const requestConfiguredFixture = createFixtureWpCli(root);
  const requestConfiguredResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      wp_cli_bin: requestConfiguredFixture,
      secret_env: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CLI_CAPTURE: requestConfiguredCapturePath,
    },
  });
  assert.equal(requestConfiguredResult.status, 0, requestConfiguredResult.stderr || requestConfiguredResult.stdout);
  const requestConfiguredCapture = readJson(requestConfiguredCapturePath);
  assert.equal(requestConfiguredCapture.argv[0], 'codebox');
  assert.equal(requestConfiguredCapture.input.parent_request.wp_cli_bin, requestConfiguredFixture);

  const sourceRoot = path.join(root, 'source-plugin');
  fs.mkdirSync(sourceRoot, { recursive: true });
  const riskyArtifacts = path.join(sourceRoot, 'artifacts');
  const riskyCapturePath = path.join(root, 'capture-risky-artifacts.json');
  const riskyResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-cli-bin',
    fixtureWpCli,
    '--agents-api',
    '/components/agents-api',
    '--data-machine',
    '/components/data-machine',
    '--data-machine-code',
    '/components/data-machine-code',
    '--mount',
    `${sourceRoot}:/wordpress/wp-content/plugins/plugin:readwrite`,
    '--artifacts',
    riskyArtifacts,
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: {
      ...process.env,
      FIXTURE_WP_CLI_CAPTURE: riskyCapturePath,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(riskyResult.status, 0, riskyResult.stderr || riskyResult.stdout);
  assert.match(riskyResult.stderr, /may be captured recursively/);

  const missingSecretResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-cli-bin',
    fixtureWpCli,
    '--agents-api',
    '/components/agents-api',
    '--data-machine',
    '/components/data-machine',
    '--data-machine-code',
    '/components/data-machine-code',
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: {
      ...process.env,
      FIXTURE_WP_CLI_CAPTURE: path.join(root, 'capture-missing-secret.json'),
      OPENCODE_API_KEY: '',
    },
  });
  assert.notEqual(missingSecretResult.status, 0);
  assert.match(missingSecretResult.stderr, /Required WP Codebox secret environment variable missing: OPENCODE_API_KEY/);

  console.log('Homeboy WP Codebox task runner smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
