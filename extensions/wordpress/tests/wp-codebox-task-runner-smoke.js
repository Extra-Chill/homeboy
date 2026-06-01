'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

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
const path = require('node:path');
const out = process.env.FIXTURE_WP_CODEBOX_CAPTURE;
const recipeIndex = process.argv.indexOf('--recipe');
const recipePath = recipeIndex >= 0 ? process.argv[recipeIndex + 1] : '';
const recipe = recipePath ? JSON.parse(fs.readFileSync(recipePath, 'utf8')) : null;
fs.writeFileSync(out, JSON.stringify({ argv: process.argv.slice(2), recipe }, null, 2));
process.stdout.write(JSON.stringify({
  success: true,
  artifacts: {
    id: 'artifact-bundle-sha256-fixture',
    directory: path.dirname(out),
  },
}));
`);
  fs.chmodSync(binPath, mode);
  return binPath;
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-task-runner-'));

try {
  const capturePath = path.join(root, 'capture.json');
  const fixtureWpCodebox = createFixtureWpCodebox(root);
  const providerPluginPath = path.join(root, 'example-provider@feature-branch');
  fs.mkdirSync(providerPluginPath, { recursive: true });
  fs.writeFileSync(path.join(providerPluginPath, 'provider-main.php'), '<?php\n/**\n * Plugin Name: Example Provider\n */');
  const codexProviderPluginPath = path.join(root, 'ai-provider-for-openai');
  fs.mkdirSync(codexProviderPluginPath, { recursive: true });
  fs.writeFileSync(path.join(codexProviderPluginPath, 'ai-provider-for-openai.php'), '<?php\n/**\n * Plugin Name: AI Provider for OpenAI\n */');
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
      FIXTURE_WP_CODEBOX_CAPTURE: capturePath,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  const output = JSON.parse(result.stdout);
  assert.equal(output.success, true);

  const captured = readJson(capturePath);
  assert.deepEqual(captured.argv.slice(0, 4), ['recipe-run', '--recipe', captured.argv[2], '--json']);
  assert.equal(captured.argv.includes('--artifacts'), true);

  const recipe = captured.recipe;
  assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
  assert.deepEqual(recipe.inputs.workspaces.map((workspace) => workspace.seed.slug), [
    'agents-api',
    'homeboy',
    'homeboy-extensions',
  ]);
  assert.deepEqual(recipe.inputs.workspaces[0].seed.excludePaths, [
    '.git',
    '.homeboy',
    '.homeboy-bin',
    '.homeboy-build',
    '.datamachine',
    '.DS_Store',
    '._*',
    '.env*',
    'node_modules',
    'target',
    'vendor',
  ]);
  assert.deepEqual(recipe.inputs.workspaces.map((workspace) => workspace.mode), [
    'readwrite',
    'readwrite',
    'readwrite',
  ]);
  assert.deepEqual(recipe.inputs.secretEnv, ['OPENCODE_API_KEY']);
  assert.equal(recipe.inputs.mounts[0].source, '/repo/plugin');
  assert.equal(recipe.runtime.stack.mounts[0].source, '/components/php-ai-client');
  assert.equal(recipe.runtime.stack.mounts[0].target, '/wordpress/wp-includes/php-ai-client');
  assert.equal(recipe.runtime.stack.mounts[0].metadata.component, 'php-ai-client');
  assert.equal(recipe.runtime.stack.mounts[1].source, '/components/wordpress-develop');
  assert.equal(recipe.runtime.stack.mounts[1].target, '/wordpress');
  assert.equal(recipe.runtime.stack.mounts[1].metadata.kind, 'homeboy-runtime-stack');
  assert.equal(recipe.runtime.overlays[0].type, 'bundled-library');
  assert.equal(recipe.runtime.overlays[0].library, 'php-ai-client');
  assert.equal(recipe.runtime.overlays[0].source, '/components/php-ai-client');
  assert.equal(recipe.runtime.overlays[1].type, 'wordpress-scoped-bundle');
  assert.equal(recipe.runtime.overlays[1].source, '/components/wordpress-scoped-bundle');
  assert.equal(recipe.inputs.extraPlugins[0].slug, 'agents-api');
  assert.equal(recipe.inputs.extraPlugins[1].slug, 'data-machine');
  assert.equal(recipe.inputs.extraPlugins[2].slug, 'data-machine-code');
  assert.equal(recipe.inputs.extraPlugins[3].slug, 'example-provider');
  assert.equal(recipe.inputs.extraPlugins[3].pluginFile, 'example-provider/provider-main.php');

  const step = recipe.workflow.steps[0];
  assert.equal(step.command, 'wp-codebox.agent-sandbox-run');
  assert.equal(step.args.includes('provider=opencode'), true);
  assert.equal(step.args.includes('model=opencode-go/kimi-k2.6'), true);
  assert.equal(step.args.includes('max-turns=80'), true);
  assert.equal(step.args.includes('timeout-seconds=7200'), true);
  assert.equal(step.args.includes('provider-plugin-slugs=example-provider'), true);
  assert.equal(step.args.some((arg) => arg.startsWith('session-id=')), false);

  const taskArg = step.args.find((arg) => arg.startsWith('task='));
  assert.ok(taskArg);
  const task = JSON.parse(taskArg.slice('task='.length));
  assert.equal(task.schema, 'homeboy/wp-codebox-audit-task/v1');
  assert.equal(task.sandbox_session_id, 'homeboy-audit-fixture-session');
  assert.equal(task.orchestrator.issue_url, 'https://github.com/Extra-Chill/homeboy-extensions/issues/775');
  assert.equal(task.audit_findings[0].id, 'finding-1');
  assert.match(task.task.prompt, /`agents-api`, `homeboy`, `homeboy-extensions`/);

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
      provider_plugin_paths: [codexProviderPluginPath],
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
  assert.equal(codexRecipe.inputs.extraPlugins[3].slug, 'ai-provider-for-openai');
  assert.equal(codexRecipe.inputs.extraPlugins[3].pluginFile, 'ai-provider-for-openai/ai-provider-for-openai.php');
  assert.equal(codexRecipe.workflow.steps[0].args.includes('provider=codex'), true);
  assert.equal(codexRecipe.workflow.steps[0].args.includes('model=gpt-5.5'), true);
  assert.equal(codexRecipe.workflow.steps[0].args.includes('provider-plugin-slugs=ai-provider-for-openai'), true);
  const serializedCodexRecipe = JSON.stringify(codexRecipe);
  assert(!serializedCodexRecipe.includes('access-token-value'));
  assert(!serializedCodexRecipe.includes('refresh-token-value'));
  assert(!serializedCodexRecipe.includes('wp-ai-gateway'));

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
  const defaultArtifactsIndex = nonExecutableCapture.argv.indexOf('--artifacts');
  assert.notEqual(defaultArtifactsIndex, -1);
  assert.equal(pathInside(root, nonExecutableCapture.argv[defaultArtifactsIndex + 1]), false);

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
