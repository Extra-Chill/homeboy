'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function createFixtureWpCodebox(root, mode = 0o755) {
  const binPath = path.join(root, 'fixture-wp-codebox.js');
  fs.writeFileSync(binPath, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const out = process.env.FIXTURE_WP_CODEBOX_CAPTURE;
const inputArg = process.argv.find((arg) => arg.startsWith('--input-file='));
const inputPath = inputArg ? inputArg.slice('--input-file='.length) : '';
const input = inputPath ? JSON.parse(fs.readFileSync(inputPath, 'utf8')) : null;
const isDatamachineBundle = Boolean(input.datamachine_bundle && Object.keys(input.datamachine_bundle).length);
const agentResult = isDatamachineBundle && !process.env.FIXTURE_WP_CODEBOX_INCOMPLETE_DATAMACHINE
  ? { metrics: { config_present: 1 }, metadata: { engine_data: { store_idea_agent: { issue_number: 123 } } } }
  : { status: 'completed' };
fs.writeFileSync(out, JSON.stringify({ argv: process.argv.slice(2), input, datamachineConfig: JSON.parse(process.env.HOMEBOY_DATAMACHINE_AGENT_CONFIG || '{}') }, null, 2));
process.stdout.write(JSON.stringify({
  success: !isDatamachineBundle,
  schema: 'wp-codebox/agent-task-run/v1',
  session: {
    schema: 'wp-codebox/sandbox-session/v1',
    id: input.sandbox_session_id,
    status: isDatamachineBundle ? 'failed' : 'completed',
    artifacts: { bundle_id: 'artifact-bundle-sha256-fixture', path: input.artifacts_path, preview_url: 'https://preview.example.test/' + input.sandbox_session_id },
    orchestrator: input.orchestrator
  },
  task_input: input,
  artifacts: input.artifacts_path,
  run: isDatamachineBundle ? {} : { agentResult },
  executions: [{ recipeCommand: 'wp-codebox.agent-sandbox-run', exitCode: 0, stdout: JSON.stringify({ status: 'completed', output: JSON.stringify(agentResult) }) }],
}));
`);
  fs.chmodSync(binPath, mode);
  return binPath;
}

function pathInside(parent, candidate) {
  const relative = path.relative(parent, candidate);
  return relative === '' || (Boolean(relative) && !relative.startsWith('..') && !path.isAbsolute(relative));
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-task-runner-'));

try {
  const capturePath = path.join(root, 'capture.json');
  const fixtureWpCodebox = createFixtureWpCodebox(root);
  const providerPluginPath = path.join(root, 'example-provider@feature-branch');
  const workspaceRoot = path.join(root, 'wp-coding-agents@proof-homeboy-fanout-a');
  fs.mkdirSync(providerPluginPath, { recursive: true });
  fs.mkdirSync(workspaceRoot, { recursive: true });

  const request = {
    schema: 'wp-codebox/task-input/v1',
    goal: 'Fix the finding.',
    target: { root: workspaceRoot, mode: 'readwrite' },
    expected_artifacts: ['patch'],
    policy: { kind: 'audit-remediation' },
    context: { source: 'homeboy-smoke' },
    sandbox_session_id: 'homeboy-audit-fixture-session',
    provider: 'opencode',
    model: 'opencode-go/kimi-k2.6',
    provider_plugin_paths: [providerPluginPath],
    runtime_stack_mounts: [{ source: '/components/php-ai-client', target: '/wordpress/wp-includes/php-ai-client', mode: 'readonly' }],
    runtime_overlays: [{ kind: 'bundled-library', library: 'php-ai-client' }],
    secret_env: ['OPENCODE_API_KEY'],
    orchestrator: { agent_task_id: 'agent-task-123', run_id: 'run-123' },
  };

  const result = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin', fixtureWpCodebox,
    '--agents-api', '/components/agents-api',
    '--data-machine', '/components/data-machine',
    '--data-machine-code', '/components/data-machine-code',
    '--mount', '/repo/plugin:/wordpress/wp-content/plugins/plugin:readwrite',
    '--runtime-stack-mount', '/components/wordpress-develop:/wordpress:readonly',
    '--max-turns', '80',
    '--task-timeout-seconds', '7200',
    '--artifacts', path.join(root, 'artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: capturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  const output = JSON.parse(result.stdout);
  assert.equal(output.schema, 'wp-codebox/agent-task-run/v1');
  assert.equal(output.success, true);
  assert.equal(output.session.id, 'homeboy-audit-fixture-session');
  assert.equal(output.artifacts, path.join(root, 'artifacts'));

  const captured = readJson(capturePath);
  assert.deepEqual(captured.argv.slice(0, 1), ['agent-task-run']);
  assert(captured.argv.some((arg) => arg.startsWith('--input-file=')));
  assert.equal(captured.argv.includes('--json'), true);
  assert(!captured.argv.includes('recipe-run'));
  assert.equal(captured.input.schema, 'wp-codebox/task-input/v1');
  assert.equal(captured.input.goal, 'Fix the finding.');
  assert.equal(captured.input.provider, 'opencode');
  assert.equal(captured.input.model, 'opencode-go/kimi-k2.6');
  assert.deepEqual(captured.input.secret_env, ['OPENCODE_API_KEY']);
  assert.equal(captured.input.provider_plugin_paths[0], providerPluginPath);
  assert.equal(captured.input.runtime_stack_mounts[0].source, '/components/php-ai-client');
  assert.equal(captured.input.runtime_stack_mounts[1].source, '/components/wordpress-develop');
  assert.equal(captured.input.mounts[0].source, '/repo/plugin');
  assert.equal(captured.input.agents_api_path, '/components/agents-api');
  assert.equal(captured.input.data_machine_path, '/components/data-machine');
  assert.equal(captured.input.data_machine_code_path, '/components/data-machine-code');
  assert(!JSON.stringify(captured.input).includes('redacted-test-key'));

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
    '--wp-codebox-bin', fixtureWpCodebox,
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
      FIXTURE_WP_CODEBOX_CAPTURE: codexCapturePath,
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
  assert(!JSON.stringify(codexInput).includes('access-token-value'));
  assert(!JSON.stringify(codexInput).includes('refresh-token-value'));

  const sourceRoot = path.join(root, 'source-plugin');
  fs.mkdirSync(sourceRoot, { recursive: true });
  const riskyArtifacts = path.join(sourceRoot, 'artifacts');
  const riskyResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin', fixtureWpCodebox,
    '--mount', `${sourceRoot}:/wordpress/wp-content/plugins/plugin:readwrite`,
    '--artifacts', riskyArtifacts,
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: path.join(root, 'capture-risky-artifacts.json'), OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(riskyResult.status, 0, riskyResult.stderr || riskyResult.stdout);
  assert.match(riskyResult.stderr, /may be captured recursively/);

  const datamachineCapturePath = path.join(root, 'capture-datamachine.json');
  const datamachineResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'datamachine-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      datamachine_bundle: {
        bundle_path: '/workspace/wp-site-generator/bundles/store-idea-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.store_idea_agent.issue_number' },
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: datamachineCapturePath,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(datamachineResult.status, 0, datamachineResult.stderr || datamachineResult.stdout);
  const datamachineCapture = readJson(datamachineCapturePath);
  assert.equal(datamachineCapture.argv[0], 'agent-task-run');
  assert(datamachineCapture.input.secret_env.includes('HOMEBOY_DATAMACHINE_AGENT_CONFIG'));
  assert.equal(datamachineCapture.input.datamachine_bundle.engine_data_outputs.issue_number, 'metadata.engine_data.store_idea_agent.issue_number');
  assert.equal(datamachineCapture.datamachineConfig.engine_data_outputs.issue_number, 'metadata.engine_data.store_idea_agent.issue_number');
  const datamachineOutput = JSON.parse(datamachineResult.stdout);
  assert.equal(datamachineOutput.success, true);
  assert.equal(datamachineOutput.session.status, 'completed');
  assert.equal(datamachineOutput.run.agentResult.scenarios[0].metadata.engine_data.store_idea_agent.issue_number, 123);

  const incompleteDatamachineCapturePath = path.join(root, 'capture-incomplete-datamachine.json');
  const incompleteDatamachineResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'incomplete-datamachine-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      datamachine_bundle: {
        bundle_path: '/workspace/wp-site-generator/bundles/store-idea-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.store_idea_agent.issue_number' },
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: incompleteDatamachineCapturePath,
      FIXTURE_WP_CODEBOX_INCOMPLETE_DATAMACHINE: '1',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(incompleteDatamachineResult.status, 1, incompleteDatamachineResult.stderr || incompleteDatamachineResult.stdout);
  const incompleteDatamachineOutput = JSON.parse(incompleteDatamachineResult.stdout);
  assert.equal(incompleteDatamachineOutput.success, false);
  assert.equal(incompleteDatamachineOutput.session.status, 'failed');
  assert.equal(incompleteDatamachineOutput.diagnostics[0].class, 'datamachine.workload.incomplete');
  assert.equal(incompleteDatamachineOutput.diagnostics[0].data.reason, 'missing_scenarios');

  const nonExecutableCapturePath = path.join(root, 'capture-non-executable.json');
  const nonExecutableFixture = createFixtureWpCodebox(root, 0o644);
  const nonExecutableResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin', nonExecutableFixture,
    '--agents-api', '/components/agents-api',
    '--data-machine', '/components/data-machine',
    '--data-machine-code', '/components/data-machine-code',
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: nonExecutableCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(nonExecutableResult.status, 0, nonExecutableResult.stderr || nonExecutableResult.stdout);
  const nonExecutableCapture = readJson(nonExecutableCapturePath);
  assert.equal(nonExecutableCapture.argv[0], 'agent-task-run');
  assert.equal(pathInside(root, nonExecutableCapture.input.artifacts_path), false);

  const missingSecretResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin', fixtureWpCodebox,
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: path.join(root, 'capture-missing-secret.json'), OPENCODE_API_KEY: '' },
  });
  assert.notEqual(missingSecretResult.status, 0);
  assert.match(missingSecretResult.stderr, /Required WP Codebox secret environment variable missing: OPENCODE_API_KEY/);

  console.log('Homeboy WP Codebox task runner smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
