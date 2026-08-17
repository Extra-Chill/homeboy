'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const wpCodeboxTaskRunner = path.join(
  __dirname,
  '..',
  '..',
  'agent-runtimes',
  'wp-codebox',
  'scripts',
  'agent',
  'homeboy-wp-codebox-task-runner.cjs'
);
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(
  __dirname,
  '..',
  '..',
  'tests',
  'fixtures',
  'wp-codebox-core-runtime-contract.cjs'
);

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function createFixtureWpCodebox(root, version = '0.21.0') {
  fs.mkdirSync(root, { recursive: true });
  const binPath = path.join(root, 'fixture-wp-codebox.js');
  fs.writeFileSync(binPath, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const capturePath = process.env.FIXTURE_WP_CODEBOX_CAPTURE;
if (process.argv.includes('--version')) {
  process.stdout.write(${JSON.stringify(version)});
  process.exit(0);
}
const inputArg = process.argv.find((arg) => arg.startsWith('--input-file='));
const inputPath = inputArg ? inputArg.slice('--input-file='.length) : process.argv[process.argv.indexOf('--input-file') + 1];
const input = JSON.parse(fs.readFileSync(inputPath, 'utf8'));
const taskInput = input.task_input || input;
fs.writeFileSync(capturePath, JSON.stringify({ argv: process.argv.slice(2), input }, null, 2));
process.stdout.write(JSON.stringify({
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  session: {
    schema: 'wp-codebox/sandbox-session/v1',
    id: taskInput.sandbox_session_id,
    status: 'completed',
    artifacts: { path: input.artifacts_path, preview_url: 'https://preview.example.test/' + taskInput.sandbox_session_id },
    orchestrator: taskInput.orchestrator
  },
  task_input: taskInput,
  artifacts: input.artifacts_path,
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    result: { outputs: { fallback_output: 'from-public-envelope' } }
  },
  executions: [{
    recipeCommand: 'wp-codebox.agent-sandbox-run',
    exitCode: 0,
    stdout: JSON.stringify({
      status: 'completed',
      output: JSON.stringify({
        success: true,
        summary: 'Created review artifact.',
        outputs: { review_url: 'https://example.test/review' },
        diagnostics: [{ class: 'agent_runtime.output', message: 'Semantic outputs captured.' }]
      })
    })
  }]
}));
`);
  fs.chmodSync(binPath, 0o755);
  return binPath;
}

function runTaskRunner(request, args, env) {
  return spawnSync(process.execPath, [wpCodeboxTaskRunner, ...args], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: { ...process.env, ...env },
  });
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-task-runner-'));
try {
  const capturePath = path.join(root, 'capture.json');
  const isolatedRuntimeEnv = { HOMEBOY_WP_CODEBOX_INSTALL_DIR: path.join(root, 'missing-managed-runtime') };
  const fixtureWpCodebox = createFixtureWpCodebox(root);
  const providerPluginPath = path.join(root, 'example-provider');
  const workspaceRoot = path.join(root, 'workspace');
  fs.mkdirSync(providerPluginPath, { recursive: true });
  fs.mkdirSync(workspaceRoot, { recursive: true });
  fs.writeFileSync(path.join(providerPluginPath, 'example-provider.php'), '<?php\n/* Plugin Name: Example Provider */\n');

  const request = {
    schema: 'wp-codebox/task-input/v1',
    goal: 'Fix the finding.',
    target: { root: workspaceRoot, mode: 'readwrite' },
    sandbox_session_id: 'homeboy-audit-fixture-session',
    provider: 'opencode',
    model: 'opencode-go/kimi-k2.6',
    provider_plugin_paths: [providerPluginPath],
    secret_env: ['OPENCODE_API_KEY'],
    orchestrator: { agent_task_id: 'agent-task-123', run_id: 'run-123' },
    agent_bundle: {
      slug: 'example-agent',
      runtime_bundle_ability: 'example/run-agent-bundle',
      runtime_output_projections: { review_url: 'outputs.review_url' },
    },
  };

  const staleCapturePath = path.join(root, 'stale-capture.json');
  const staleWpCodebox = createFixtureWpCodebox(path.join(root, 'stale'), '0.19.0');
  const stale = runTaskRunner(request, [
    '--wp-codebox-bin', staleWpCodebox,
    '--artifacts', path.join(root, 'stale-artifacts'),
  ], { ...isolatedRuntimeEnv, FIXTURE_WP_CODEBOX_CAPTURE: staleCapturePath, OPENCODE_API_KEY: 'redacted-test-key' });
  assert.equal(stale.status, 1, stale.stderr || stale.stdout);
  const stalePayload = JSON.parse(stale.stdout);
  assert.equal(stalePayload.diagnostics[0].class, 'codebox.preflight.wp_codebox_version_too_old', stale.stdout);
  assert.equal(stalePayload.diagnostics[0].data.selected.version, '0.19.0');
  assert.equal(stalePayload.diagnostics[0].data.required_version, '0.21.0');
  assert.equal(fs.existsSync(staleCapturePath), false, 'a rejected runtime must not start a recipe');

  const environmentalPrecedence = runTaskRunner(request, [
    '--artifacts', path.join(root, 'environmental-precedence-artifacts'),
  ], {
    ...isolatedRuntimeEnv,
    HOMEBOY_WP_CODEBOX_BIN: staleWpCodebox,
    WP_CODEBOX_BIN: fixtureWpCodebox,
    FIXTURE_WP_CODEBOX_CAPTURE: path.join(root, 'environmental-precedence-capture.json'),
    OPENCODE_API_KEY: 'redacted-test-key',
  });
  assert.equal(environmentalPrecedence.status, 1, environmentalPrecedence.stderr || environmentalPrecedence.stdout);
  const environmentalPrecedencePayload = JSON.parse(environmentalPrecedence.stdout);
  assert.equal(environmentalPrecedencePayload.diagnostics[0].data.selected.path, staleWpCodebox);
  assert.equal(fs.existsSync(path.join(root, 'environmental-precedence-capture.json')), false, 'the first explicit environment override must win before a recipe starts');

  const normalized = runTaskRunner(request, [
    '--wp-codebox-bin', fixtureWpCodebox,
    '--artifacts', path.join(root, 'artifacts'),
  ], { ...isolatedRuntimeEnv, FIXTURE_WP_CODEBOX_CAPTURE: capturePath, OPENCODE_API_KEY: 'redacted-test-key' });
  assert.equal(normalized.status, 0, normalized.stderr || normalized.stdout);
  const normalizedPayload = JSON.parse(normalized.stdout);
  assert.equal(normalizedPayload.success, true);
  assert.equal(normalizedPayload.outputs.review_url, 'https://example.test/review');
  assert.equal(normalizedPayload.diagnostics.some((diagnostic) => diagnostic.class === 'agent_runtime.output'), true);

  const captured = readJson(capturePath);
  assert.equal(captured.argv[0], 'run-agent-task');
  assert.equal(captured.argv.includes('--json'), true);
  assert.equal(captured.input.schema, 'wp-codebox/run-agent-task/v1');
  assert.equal(captured.input.task_input.provider, 'opencode');
  assert.equal(captured.input.task_input.model, 'opencode-go/kimi-k2.6');
  assert.deepEqual(captured.input.task_input.secret_env, ['OPENCODE_API_KEY']);

  const contractCanaryCapture = path.join(root, 'capture-contract-canary.json');
  const contractCanary = runTaskRunner({ ...request, agent_bundle: undefined }, [
    '--wp-codebox-bin', fixtureWpCodebox,
    '--mount', `${workspaceRoot}:/wordpress/wp-content/plugins/example:readwrite`,
    '--artifacts', path.join(root, 'contract-artifacts'),
  ], { ...isolatedRuntimeEnv, FIXTURE_WP_CODEBOX_CAPTURE: contractCanaryCapture, OPENCODE_API_KEY: 'redacted-test-key' });
  assert.equal(contractCanary.status, 0, contractCanary.stderr || contractCanary.stdout);
  const contractInput = readJson(contractCanaryCapture).input;
  assert.equal(contractInput.schema, 'wp-codebox/run-agent-task/v1');
  assert.equal(contractInput.version, 1);
  assert.equal(contractInput.task_input.mounts[0].target, '/wordpress/wp-content/plugins/example');
  assert.equal(contractInput.artifacts_path, path.join(root, 'contract-artifacts'));
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
