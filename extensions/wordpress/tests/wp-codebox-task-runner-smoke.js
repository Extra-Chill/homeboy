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

function createFixtureWpCodebox(root, mode = 0o755) {
  const binPath = path.join(root, 'fixture-wp-codebox.js');
  fs.writeFileSync(binPath, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const out = process.env.FIXTURE_WP_CODEBOX_CAPTURE;
const recipeIndex = process.argv.indexOf('--recipe');
const recipePath = recipeIndex >= 0 ? process.argv[recipeIndex + 1] : '';
const artifactsIndex = process.argv.indexOf('--artifacts');
const artifacts = artifactsIndex >= 0 ? process.argv[artifactsIndex + 1] : '';
const recipe = recipePath ? JSON.parse(fs.readFileSync(recipePath, 'utf8')) : null;
const sessionArg = recipe.workflow.steps[0].args.find((arg) => arg.startsWith('session-id=')) || 'session-id=fixture-session';
const sessionId = sessionArg.slice('session-id='.length);
const isDatamachineBundle = recipe.workflow.steps[0].args.some((arg) => arg.startsWith('code-file='));
const agentResult = isDatamachineBundle && !process.env.FIXTURE_WP_CODEBOX_INCOMPLETE_DATAMACHINE
  ? { scenarios: [{ id: 'datamachine-agent', metadata: { engine_data: { store_idea_agent: { issue_number: 123 } } } }] }
  : { status: 'completed' };
fs.writeFileSync(out, JSON.stringify({ argv: process.argv.slice(2), recipe, datamachineConfig: JSON.parse(process.env.HOMEBOY_DATAMACHINE_AGENT_CONFIG || '{}') }, null, 2));
process.stdout.write(JSON.stringify({
  success: true,
  schema: 'wp-codebox/recipe-run/v1',
  recipePath,
  runtime: { preview: { url: 'https://preview.example.test/' + sessionId } },
  executions: [{ recipeCommand: 'wp-codebox.agent-sandbox-run', exitCode: 0, stdout: JSON.stringify({ status: 'completed' }) }],
  artifacts: { id: 'artifact-bundle-sha256-fixture', directory: artifacts },
  agentResult,
}));
`);
  fs.chmodSync(binPath, mode);
  return binPath;
}

function createFixtureComposer(root) {
  const binDir = path.join(root, 'bin');
  const binPath = path.join(binDir, 'composer');
  fs.mkdirSync(binDir, { recursive: true });
  fs.writeFileSync(binPath, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const path = require('node:path');
fs.mkdirSync(path.join(process.cwd(), 'vendor'), { recursive: true });
fs.writeFileSync(path.join(process.cwd(), 'vendor', 'autoload.php'), '<?php // fixture autoload');
process.stdout.write('fixture composer install\\n');
`);
  fs.chmodSync(binPath, 0o755);
  return binDir;
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
      kind: 'bundled-library',
      library: 'php-ai-client',
      source: '/components/php-ai-client',
      target: '/wordpress/wp-includes/php-ai-client',
      strategy: 'wordpress-scoped-bundle',
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
    audit_findings: [{
      id: 'finding-1',
      kind: 'wordpress.phpcs.fixable',
      file: 'src/Example.php',
      line: 10,
      message: 'Fix spacing.',
      severity: 'warning',
    }],
    task: {
      title: 'Fix Homeboy audit batch PHPCS Formatting/Auto Fix!',
      prompt: 'Fix the finding.',
      expected_artifacts: ['patch'],
      policy: { kind: 'audit-remediation' },
      context: { source: 'homeboy-smoke' },
      workspace: { root: workspaceRoot, mode: 'readwrite' },
    },
  };

  const result = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--agents-api',
    '/components/agents-api',
    '--data-machine',
    '/components/data-machine',
    '--data-machine-code',
    '/components/data-machine-code',
    '--mount',
    '/repo/plugin:/wordpress/wp-content/plugins/plugin:readwrite',
    '--runtime-stack-mount',
    '/components/wordpress-develop:/wordpress:readonly',
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
      FIXTURE_WP_CODEBOX_CAPTURE: capturePath,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  const output = JSON.parse(result.stdout);
  assert.equal(output.schema, 'wp-codebox/agent-task-run/v1');
  assert.equal(output.success, true);
  assert.equal(output.session.id, 'homeboy-audit-fixture-session');
  assert.equal(output.artifacts, path.join(root, 'artifacts'));
  assert.equal(output.run.agentResult.status, 'completed');

  const captured = readJson(capturePath);
  assert.deepEqual(captured.argv.slice(0, 4), ['recipe-run', '--recipe', captured.argv[2], '--json']);
  assert.equal(captured.argv.includes('--artifacts'), true);
  assert(!captured.argv.includes('codebox'));
  assert(!captured.argv.includes('run-agent-task'));

  const recipe = captured.recipe;
  assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
  assert.equal(recipe.workflow.steps[0].command, 'wp-codebox.agent-sandbox-run');
  assert(recipe.workflow.steps[0].args.includes('agent=wp-codebox-sandbox'));
  assert(recipe.workflow.steps[0].args.includes('mode=sandbox'));
  assert(recipe.workflow.steps[0].args.includes('provider=opencode'));
  assert(recipe.workflow.steps[0].args.includes('model=opencode-go/kimi-k2.6'));
  assert(recipe.workflow.steps[0].args.includes('max-turns=80'));
  assert(recipe.workflow.steps[0].args.includes('timeout-seconds=7200'));
  assert.equal(recipe.inputs.extraPlugins.find((plugin) => plugin.slug === 'agents-api').source, '/components/agents-api');
  assert.equal(recipe.inputs.extraPlugins.find((plugin) => plugin.slug === 'data-machine').source, '/components/data-machine');
  assert.equal(recipe.inputs.extraPlugins.find((plugin) => plugin.slug === 'data-machine-code').source, '/components/data-machine-code');
  assert.equal(recipe.inputs.extraPlugins.find((plugin) => plugin.slug === 'example-provider').source, providerPluginPath);
  assert.deepEqual(recipe.inputs.secretEnv, ['OPENCODE_API_KEY']);
  assert.equal(recipe.inputs.mounts[0].source, '/repo/plugin');
  assert.equal(recipe.inputs.mounts[1].source, workspaceRoot);
  assert.equal(recipe.inputs.mounts[1].target, '/workspace/wp-coding-agents');
  assert.deepEqual(recipe.inputs.mounts[1].metadata, {
    kind: 'homeboy-agent-task-workspace',
    slug: 'wp-coding-agents',
    workspaceRef: 'wp-coding-agents@proof-homeboy-fanout-a',
    repo: 'wp-coding-agents',
  });
  assert.equal(recipe.runtime.stack.mounts[0].source, '/components/php-ai-client');
  assert.equal(recipe.runtime.stack.mounts[1].source, '/components/wordpress-develop');
  assert.equal(recipe.runtime.overlays[0].kind, 'bundled-library');
  assert(!JSON.stringify(recipe).includes('redacted-test-key'));

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
    '--wp-codebox-bin',
    fixtureWpCodebox,
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
      FIXTURE_WP_CODEBOX_CAPTURE: codexCapturePath,
      AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'access-token-value',
      AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'refresh-token-value',
      AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '4102444800',
      AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'account-id-value',
      AI_PROVIDER_OPENAI_CODEX_FEDRAMP: '0',
    },
  });
  assert.equal(codexResult.status, 0, codexResult.stderr || codexResult.stdout);
  const codexRecipe = readJson(codexCapturePath).recipe;
  assert.deepEqual(codexRecipe.inputs.secretEnv, codexSecretEnv);
  assert(codexRecipe.workflow.steps[0].args.includes('provider=codex'));
  assert(codexRecipe.workflow.steps[0].args.includes('model=gpt-5.5'));
  assert.equal(codexRecipe.inputs.extraPlugins.find((plugin) => plugin.slug === 'ai-provider-for-openai').source, '/components/ai-provider-for-openai');
  const serializedCodexRecipe = JSON.stringify(codexRecipe);
  assert(!serializedCodexRecipe.includes('access-token-value'));
  assert(!serializedCodexRecipe.includes('refresh-token-value'));
  assert(!serializedCodexRecipe.includes('wp-ai-gateway'));

  const composerCapturePath = path.join(root, 'capture-composer.json');
  const composerPluginPath = path.join(root, 'data-machine-with-composer');
  fs.mkdirSync(composerPluginPath, { recursive: true });
  fs.writeFileSync(path.join(composerPluginPath, 'composer.json'), JSON.stringify({ name: 'fixture/data-machine' }));
  fs.writeFileSync(path.join(composerPluginPath, 'data-machine.php'), '<?php require __DIR__ . "/vendor/autoload.php";');
  const composerResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--data-machine',
    composerPluginPath,
    '--artifacts',
    path.join(root, 'composer-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      execution_kind: 'datamachine_bundle',
      homeboy_extensions: path.join(__dirname, '..'),
      datamachine_bundle: {
        bundle_path: '/workspace/wp-site-generator/bundles/store-idea-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.store_idea_agent.issue_number' },
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: composerCapturePath,
      PATH: `${createFixtureComposer(root)}${path.delimiter}${process.env.PATH}`,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(composerResult.status, 0, composerResult.stderr || composerResult.stdout);
  const composerRecipe = readJson(composerCapturePath).recipe;
  const codeFileArg = composerRecipe.workflow.steps[0].args.find((arg) => arg.startsWith('code-file='));
  assert.equal(codeFileArg, 'code-file=/homeboy-extension/scripts/agent/homeboy-datamachine-agent-workload-wrapper.php');
  assert.match(fs.readFileSync(path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-datamachine-agent-workload-wrapper.php'), 'utf8'), /\/homeboy-extension\/scripts\/agent\/datamachine-agent-workload\.php/);
  assert(composerRecipe.inputs.secretEnv.includes('HOMEBOY_DATAMACHINE_AGENT_CONFIG'));
  assert.equal(readJson(composerCapturePath).datamachineConfig.engine_data_outputs.issue_number, 'metadata.engine_data.store_idea_agent.issue_number');
  const preparedDataMachine = composerRecipe.inputs.extraPlugins.find((plugin) => plugin.slug === 'data-machine');
  assert.notEqual(preparedDataMachine.source, composerPluginPath);
  assert(pathInside(path.join(root, 'composer-artifacts', 'prepared-plugins'), fs.realpathSync(preparedDataMachine.source)));
  assert(fs.existsSync(path.join(preparedDataMachine.source, 'vendor', 'autoload.php')));
  assert(!fs.existsSync(path.join(composerPluginPath, 'vendor', 'autoload.php')));

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
      execution_kind: 'datamachine_bundle',
      homeboy_extensions: path.join(__dirname, '..'),
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
    '--wp-codebox-bin',
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
      FIXTURE_WP_CODEBOX_CAPTURE: nonExecutableCapturePath,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(nonExecutableResult.status, 0, nonExecutableResult.stderr || nonExecutableResult.stdout);
  const nonExecutableCapture = readJson(nonExecutableCapturePath);
  assert.equal(nonExecutableCapture.argv[0], 'recipe-run');
  assert.equal(pathInside(root, nonExecutableCapture.argv[nonExecutableCapture.argv.indexOf('--artifacts') + 1]), false);

  const sourceRoot = path.join(root, 'source-plugin');
  fs.mkdirSync(sourceRoot, { recursive: true });
  const riskyArtifacts = path.join(sourceRoot, 'artifacts');
  const riskyCapturePath = path.join(root, 'capture-risky-artifacts.json');
  const riskyResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
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
      FIXTURE_WP_CODEBOX_CAPTURE: riskyCapturePath,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(riskyResult.status, 0, riskyResult.stderr || riskyResult.stdout);
  assert.match(riskyResult.stderr, /may be captured recursively/);

  const missingSecretResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
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
      FIXTURE_WP_CODEBOX_CAPTURE: path.join(root, 'capture-missing-secret.json'),
      OPENCODE_API_KEY: '',
    },
  });
  assert.notEqual(missingSecretResult.status, 0);
  assert.match(missingSecretResult.stderr, /Required WP Codebox secret environment variable missing: OPENCODE_API_KEY/);

  console.log('Homeboy WP Codebox task runner smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
