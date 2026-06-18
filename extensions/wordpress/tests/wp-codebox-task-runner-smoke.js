'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');

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
if (process.env.FIXTURE_WP_CODEBOX_VALIDATION_FAILURE) {
  const generatedRoot = fs.mkdtempSync(require('node:path').join(require('node:os').tmpdir(), 'wp-codebox-agent-task-recipe-'));
  const generatedRecipePath = require('node:path').join(generatedRoot, 'recipe.json');
  fs.writeFileSync(generatedRecipePath, JSON.stringify({
    schema: 'wp-codebox/recipe/v1',
    secret: process.env.OPENCODE_API_KEY,
    workflow: { steps: [{ command: '', args: [] }] }
  }, null, 2));
  fs.writeFileSync(out, JSON.stringify({ argv: process.argv.slice(2), input }, null, 2));
  process.stderr.write('RecipeValidationError: Recipe validation failed with 2 issues.\\n');
  process.stderr.write('Issue 1: workflow.steps[0].command is required.\\n');
  process.stderr.write('Issue 2: provider plugin path is invalid.\\n');
  process.stderr.write('Generated recipe: ' + generatedRecipePath + '\\n');
  process.exit(1);
}
if (process.env.FIXTURE_WP_CODEBOX_JSON_VALIDATION_FAILURE) {
  const generatedRoot = fs.mkdtempSync(require('node:path').join(require('node:os').tmpdir(), 'wp-codebox-agent-task-recipe-'));
  const generatedRecipePath = require('node:path').join(generatedRoot, 'recipe.json');
  fs.writeFileSync(generatedRecipePath, JSON.stringify({
    schema: 'wp-codebox/recipe/v1',
    secret: process.env.OPENCODE_API_KEY,
    workflow: { steps: [{ command: '', args: [] }] }
  }, null, 2));
  fs.writeFileSync(out, JSON.stringify({ argv: process.argv.slice(2), input }, null, 2));
  process.stdout.write(JSON.stringify({
    success: false,
    schema: 'wp-codebox/agent-task-run/v1',
    status: 'failed',
    summary: 'WP Codebox agent task failed.',
    diagnostics: [{ class: 'wp-codebox.agent_task_run_failed', message: 'Recipe validation failed with 2 issues.', data: { exit_code: 1 } }],
    artifacts: input.artifacts_path,
    metadata: {
      run: {
        error: { name: 'RecipeValidationError', message: 'Recipe validation failed with 2 issues.' },
        replay: { recipePath: generatedRecipePath }
      }
    }
  }));
  process.exit(0);
}
const isAgentBundle = Boolean(input.agent_bundle && Object.keys(input.agent_bundle).length);
const isRuntimeTask = Boolean(input.runtime_task && Object.keys(input.runtime_task).length && input.runtime_task.ability !== 'datamachine/run-agent-bundle');
const bundleRun = isAgentBundle && process.env.FIXTURE_WP_CODEBOX_BUNDLE_RUN
  ? {
      schema: 'datamachine/agent-bundle-run/v1',
      success: true,
      dry_run: true,
      job_id: 1,
      job_status: 'completed',
      bundle: {
        bundle_slug: 'example-agent',
        flow_slug: 'example-manual-flow',
        pipeline_slug: 'example-pipeline',
      },
      typed_artifacts: {
        example_review: {
          schema: 'example/review-artifact/v1',
          type: 'ExampleReviewArtifact',
          payload: { slug: 'example-agent', review_ready: true },
          provenance: { bundle_slug: 'example-agent', task_id: input.orchestrator.agent_task_id },
          file_refs: [{ path: input.artifacts_path + '/example-review.json', mime: 'application/json' }]
        }
      },
      workflow: { steps: [{ step_type: 'ai' }] },
      wait_result: { success: true, terminal_state: 'completed' },
      engine_data: process.env.FIXTURE_WP_CODEBOX_BUNDLE_RUN_TOOL_RECORDERS
        ? {
            direct_step_data_packets: {
              ephemeral_step_1: [{
                metadata: {
                  step_execution_success: true,
                  tool_name: 'github_issue_publish',
                  tool_result_data: {
                    data: {
                      issue_number: 123,
                      issue_url: 'https://github.com/example-org/example-repo/issues/123'
                    }
                  }
                }
              }]
            }
          }
        : { example_agent: { issue_number: 123, issue_url: 'https://github.com/example-org/example-repo/issues/123' } }
    }
  : null;
const runtimeTaskResult = isRuntimeTask
  ? {
      agent_runtime: {
        success: true,
        input: input.runtime_task.input || {},
        result: {
          success: true,
          import_validation_result: {
            schema: 'example/import-validation-result/v1',
            artifact_type: 'ImportValidationResult',
            status: 'passed',
            counts: { fallback_blocks: 0 }
          },
          finding_packets: {
            schema: 'example/finding-packets/v1',
            artifact_type: 'FindingPacketSet',
            count: 0,
            packets: []
          }
        }
      }
    }
  : null;
const agentResult = isRuntimeTask
  ? runtimeTaskResult
  : (isAgentBundle && process.env.FIXTURE_WP_CODEBOX_FAILED_AGENT_BUNDLE
  ? { scenarios: [{ id: 'agent-bundle', metadata: { error: 'Agent bundle child job 456 did not reach a terminal state after drain; current status is pending.' } }] }
  : (isAgentBundle && process.env.FIXTURE_WP_CODEBOX_FAILED_RUNTIME_METADATA
  ? {
      outputs: {},
      scenarios: [{
        id: 'agent-bundle',
        metadata: {
          error_step_id: 'ephemeral_step_0',
          error_reason: 'ai_processing_failed',
          terminal_status: 'failed - ai_processing_failed',
          message: 'Codex OAuth refresh failed.'
        }
      }]
    }
  : (isAgentBundle && process.env.FIXTURE_WP_CODEBOX_BUNDLE_RUN
      ? {
          agent_runtime: {
            success: true,
            result: bundleRun,
          },
        }
  : (isAgentBundle && process.env.FIXTURE_WP_CODEBOX_SINGLE_RESULT
      ? {
          success: true,
          summary: 'Created issue 123.',
          outputs: {
            issue_number: 123,
            issue_url: 'https://github.com/example-org/example-repo/issues/123'
          },
          diagnostics: [{ class: 'agent_runtime.output', message: 'Semantic outputs captured.' }]
        }
  : (isAgentBundle && !process.env.FIXTURE_WP_CODEBOX_INCOMPLETE_AGENT_BUNDLE
      ? { metrics: { config_present: 1 }, metadata: { engine_data: { example_agent: { issue_number: 123 } } } }
      : { status: 'completed' })))));
fs.writeFileSync(out, JSON.stringify({
  argv: process.argv.slice(2),
  input,
  codex_env: {
    access_refreshed: process.env.AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN === 'fresh-access-token-value',
    refresh_refreshed: process.env.AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN === 'fresh-refresh-token-value',
    expires_refreshed: Number(process.env.AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT || 0) > Math.floor(Date.now() / 1000),
  },
}, null, 2));
const execution = { recipeCommand: 'wp-codebox.agent-sandbox-run', exitCode: 0, stdout: JSON.stringify({ status: 'completed', output: JSON.stringify(agentResult) }) };
const executions = [execution];
process.stdout.write(JSON.stringify({
  success: !isAgentBundle,
  schema: 'wp-codebox/agent-task-run/v1',
  status: isAgentBundle ? 'failed' : 'completed',
  session: {
    schema: 'wp-codebox/sandbox-session/v1',
    id: input.sandbox_session_id,
    status: isAgentBundle ? 'failed' : 'completed',
    artifacts: { bundle_id: 'artifact-bundle-sha256-fixture', path: input.artifacts_path, preview_url: 'https://preview.example.test/' + input.sandbox_session_id },
    orchestrator: input.orchestrator
  },
  task_input: input,
  artifacts: input.artifacts_path,
  run: isAgentBundle && process.env.FIXTURE_WP_CODEBOX_BUNDLE_RUN ? { executions } : (isAgentBundle ? {} : { agentResult }),
  executions: isAgentBundle && process.env.FIXTURE_WP_CODEBOX_BUNDLE_RUN ? [] : executions,
}));
if (process.env.FIXTURE_WP_CODEBOX_EXIT_CODE) {
  process.exitCode = Number(process.env.FIXTURE_WP_CODEBOX_EXIT_CODE);
}
`);
  fs.chmodSync(binPath, mode);
  return binPath;
}

function waitForFile(filePath) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    if (fs.existsSync(filePath)) {
      return fs.readFileSync(filePath, 'utf8').trim();
    }
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 50);
  }
  throw new Error(`Timed out waiting for ${filePath}`);
}

function createCodexOAuthServer(root) {
  const script = path.join(root, 'fixture-codex-oauth-server.js');
  const portPath = path.join(root, 'fixture-codex-oauth-port');
  const logPath = path.join(root, 'fixture-codex-oauth-requests.jsonl');
  fs.writeFileSync(script, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const http = require('node:http');
const { URLSearchParams } = require('node:url');
const portPath = process.argv[2];
const logPath = process.argv[3];
const server = http.createServer((request, response) => {
  let body = '';
  request.setEncoding('utf8');
  request.on('data', (chunk) => { body += chunk; });
  request.on('end', () => {
    const params = Object.fromEntries(new URLSearchParams(body));
    fs.appendFileSync(logPath, JSON.stringify({ method: request.method, url: request.url, params }) + '\\n');
    if (params.refresh_token === 'stale-refresh-token-value') {
      response.writeHead(401, { 'Content-Type': 'application/json' });
      response.end(JSON.stringify({ error: 'invalid_grant' }));
      return;
    }
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ access_token: 'fresh-access-token-value', refresh_token: 'fresh-refresh-token-value', expires_in: 3600 }));
  });
});
server.listen(0, '127.0.0.1', () => {
  fs.writeFileSync(portPath, String(server.address().port));
});
`);
  const child = spawn(process.execPath, [script, portPath, logPath], {
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  const port = waitForFile(portPath);
  return {
    url: `http://127.0.0.1:${port}/oauth/token`,
    logPath,
    stop() {
      child.kill();
    },
  };
}

function pathInside(parent, candidate) {
  const relative = path.relative(parent, candidate);
  return relative === '' || (Boolean(relative) && !relative.startsWith('..') && !path.isAbsolute(relative));
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-task-runner-'));
let codexOAuthServer;

try {
  codexOAuthServer = createCodexOAuthServer(root);
  const capturePath = path.join(root, 'capture.json');
  const fixtureWpCodebox = createFixtureWpCodebox(root);
  const providerPluginPath = path.join(root, 'example-provider@feature-branch');
  const preparedProviderPluginPath = path.join(root, 'artifacts', 'prepared-plugins', 'example-provider');
  const workspaceRoot = path.join(root, 'wp-coding-agents@proof-homeboy-fanout-a');
  const defaultDataMachinePath = path.join(root, 'data-machine');
  const defaultAgentsApiPath = path.join(defaultDataMachinePath, 'vendor', 'wordpress', 'agents-api');
  const defaultDataMachineCodePath = path.join(root, 'data-machine-code');
  fs.mkdirSync(providerPluginPath, { recursive: true });
  fs.writeFileSync(path.join(providerPluginPath, 'example-provider.php'), '<?php\n/* Plugin Name: Example Provider */\n');
  fs.writeFileSync(path.join(providerPluginPath, 'composer.json'), JSON.stringify({ name: 'extra-chill/example-provider' }));
  fs.mkdirSync(workspaceRoot, { recursive: true });
  fs.mkdirSync(defaultAgentsApiPath, { recursive: true });
  fs.mkdirSync(defaultDataMachineCodePath, { recursive: true });

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
    runtime_component_paths: {
      agent_runtime: '/components/data-machine',
      agent_runtime_tools: '/components/data-machine-code',
    },
    component_contracts: [{ slug: 'domain-component', path: '/workspace/domain-component', activate: true }],
    runtime_env: {
      GENERIC_PROVIDER_CONFIG: '/runtime/provider/config.json',
      XDG_DATA_HOME: '/runtime/provider/data',
    },
    ability_tools: [{ name: 'example_materialize_packet', ability: 'example/materialize-packet' }],
    runtime_state_mounts: [{
      source: '/host/provider/state.json',
      target: '/runtime/provider/state.json',
      mode: 'readonly',
      metadata: { purpose: 'provider-state' },
    }],
    runtime_config_mounts: [{
      source: '/host/provider/config.json',
      target: '/runtime/provider/config.json',
      mode: 'readonly',
      metadata: { purpose: 'provider-config' },
    }],
    runtime_stack_mounts: [{ source: '/components/php-ai-client', target: '/wordpress/wp-includes/php-ai-client', mode: 'readonly' }],
    runtime_overlays: [{ kind: 'bundled-library', library: 'php-ai-client' }],
    secret_env: ['OPENCODE_API_KEY'],
    verify_steps: [{ command: 'wordpress.phpunit', args: ['plugin-slug=data-machine'] }],
    orchestrator: { agent_task_id: 'agent-task-123', run_id: 'run-123' },
  };

  const result = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
    '--agents-api', '/components/agents-api',
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
  assert.equal(captured.input.version, 1);
  assert.equal(captured.input.goal, 'Fix the finding.');
  assert.equal(Object.hasOwn(captured.input, 'agent'), false);
  assert.equal(captured.input.sandbox_tool_policy.schema, 'wp-codebox/sandbox-tool-policy/v1');
  assert.equal(captured.input.sandbox_tool_policy.version, 1);
  assert.equal(captured.input.sandbox_tool_policy.tools[0].id, 'homeboy/no-runtime-tools');
  assert.equal(captured.input.sandbox_tool_policy.tools[0].allowed, false);
  assert.equal(captured.input.sandbox_tool_policy.tools[0].runtime.environment, 'control_plane');
  assert.equal(captured.input.sandbox_tool_policy.tools[0].runtime.capability_scope, 'control_plane');
  assert.equal(captured.input.provider, 'opencode');
  assert.equal(captured.input.model, 'opencode-go/kimi-k2.6');
  assert.deepEqual(captured.input.secret_env, ['OPENCODE_API_KEY']);
  assert.equal(captured.input.provider_plugin_paths[0], preparedProviderPluginPath);
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'example-provider').source, preparedProviderPluginPath);
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'example-provider').activate, true);
  assert.deepEqual(captured.input.runtime_env, request.runtime_env);
  assert.deepEqual(captured.input.ability_tools, request.ability_tools);
  assert.deepEqual(captured.input.runtime_state_mounts, request.runtime_state_mounts);
  assert.deepEqual(captured.input.runtime_config_mounts, request.runtime_config_mounts);
  assert.equal(captured.input.runtime_stack_mounts[0].source, '/components/php-ai-client');
  assert.equal(captured.input.runtime_stack_mounts[1].source, '/components/wordpress-develop');
  assert.equal(captured.input.mounts[0].source, '/repo/plugin');
  assert.equal(captured.input.runtime_component_paths.agents_api, '/components/agents-api');
  assert.equal(captured.input.runtime_component_paths.agent_runtime, '/components/data-machine');
  assert.equal(captured.input.runtime_component_paths.agent_runtime_tools, '/components/data-machine-code');
  assert.equal(Object.hasOwn(captured.input, 'agents_api_path'), false);
  assert.equal(Object.hasOwn(captured.input, 'data_machine_path'), false);
  assert.equal(Object.hasOwn(captured.input, 'data_machine_code_path'), false);
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'agents-api').source, '/components/agents-api');
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'data-machine').source, '/components/data-machine');
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'data-machine-code').source, '/components/data-machine-code');
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'data-machine').loadAs, 'mu-plugin');
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'data-machine-code').activate, false);
  // WP Codebox 0.8.0 mounts runtime components from `component_contracts`.
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'agents-api').path, '/components/agents-api');
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'data-machine').path, '/components/data-machine');
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'data-machine-code').path, '/components/data-machine-code');
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'data-machine').loadAs, 'mu-plugin');
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'data-machine-code').activate, false);
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'domain-component').path, '/workspace/domain-component');
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'domain-component').activate, true);

  const nestedOnlyCapturePath = path.join(root, 'capture-nested-component-contracts.json');
  const requestWithoutComponentContracts = { ...request };
  delete requestWithoutComponentContracts.component_contracts;
  const nestedOnlyResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
    '--agents-api', '/components/agents-api',
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...requestWithoutComponentContracts,
      parent_request: {
        component_contracts: [
          { slug: 'nested-only-domain-component', path: '/workspace/nested-only-domain-component', activate: true },
          { slug: 'nested-only-domain-component', path: '/workspace/nested-only-domain-component', activate: true },
        ],
      },
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: nestedOnlyCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(nestedOnlyResult.status, 0, nestedOnlyResult.stderr || nestedOnlyResult.stdout);
  const nestedOnlyInput = readJson(nestedOnlyCapturePath).input;
  assert.equal(nestedOnlyInput.component_contracts.find((contract) => contract.slug === 'nested-only-domain-component').path, '/workspace/nested-only-domain-component');
  assert.equal(nestedOnlyInput.component_contracts.find((contract) => contract.slug === 'nested-only-domain-component').activate, true);
  assert.equal(nestedOnlyInput.component_contracts.filter((contract) => contract.slug === 'nested-only-domain-component').length, 1);

  const explicitAgentCapturePath = path.join(root, 'capture-explicit-agent.json');
  const explicitAgentResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent: 'custom-sandbox-agent',
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: explicitAgentCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(explicitAgentResult.status, 0, explicitAgentResult.stderr || explicitAgentResult.stdout);
  assert.equal(readJson(explicitAgentCapturePath).input.agent, 'custom-sandbox-agent');

  // Verification gate flows through to the agent-task-run input so WP Codebox
  // can emit it as a recipe workflow.after step and fail the run if it is red.
  assert.equal(captured.input.verify_steps.length, 1);
  assert.equal(captured.input.verify_steps[0].command, 'wordpress.phpunit');
  assert.deepEqual(captured.input.verify_steps[0].args, ['plugin-slug=data-machine']);
  assert(!JSON.stringify(captured.input).includes('redacted-test-key'));

  const runtimeTaskCapturePath = path.join(root, 'capture-runtime-task.json');
  const runtimeTaskResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      sandbox_tool_policy: {
        schema: 'wp-codebox/sandbox-tool-policy/v1',
        version: 1,
        tools: [{ id: 'homeboy-canary/write-file', allowed: true }],
      },
      runtime_task: {
        ability: 'homeboy-canary/write-file',
        input: { path: '/workspace/codebox-canary/CANARY.md', content: 'after\n' },
      },
      agent_bundles: [{ source: '/workspace/bundles/canary-agent', slug: 'canary-agent' }],
      structured_artifacts: [{
        schema: 'wp-codebox/structured-artifact/v1',
        name: 'concept_packet',
        type: 'ConceptPacket',
        payload: { title: 'Canary concept' },
        metadata: {},
        provenance: { source: 'test' },
      }],
      artifact_declarations: [{
        schema: 'wp-codebox/artifact-declaration/v1',
        name: 'import_validation_result',
        type: 'ImportValidationResult',
        artifact_schema: 'example/import-validation-result/v1',
        required: false,
      }],
      workspaces: [{ target: '/workspace/codebox-canary', mode: 'readwrite' }],
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: runtimeTaskCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(runtimeTaskResult.status, 0, runtimeTaskResult.stderr || runtimeTaskResult.stdout);
  const runtimeTaskCaptured = readJson(runtimeTaskCapturePath);
  assert.equal(runtimeTaskCaptured.input.sandbox_tool_policy.tools[0].id, 'homeboy-canary/write-file');
  assert.equal(runtimeTaskCaptured.input.runtime_task.ability, 'homeboy-canary/write-file');
  assert.deepEqual(runtimeTaskCaptured.input.agent_bundles, [{ source: '/workspace/bundles/canary-agent', slug: 'canary-agent' }]);
  assert.equal(runtimeTaskCaptured.input.structured_artifacts[0].name, 'concept_packet');
  assert.equal(runtimeTaskCaptured.input.artifact_declarations[0].name, 'import_validation_result');
  assert.equal(runtimeTaskCaptured.input.artifact_declarations[0].required, false);
  assert.equal(runtimeTaskCaptured.input.workspaces[0].target, '/workspace/codebox-canary');

  const bridgeCapturePath = path.join(root, 'capture-runtime-tool-bridge.json');
  const bridgeResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      sandbox_tool_policy: {
        schema: 'wp-codebox/sandbox-tool-policy/v1',
        version: 1,
        tools: [{
          id: 'github_issue_publish',
          runtime_tool_id: 'github_issue_publish',
          execution_location: 'control_plane',
          allowed: false,
          runtime: { environment: 'control_plane', capability_scope: 'control_plane' },
        }],
        metadata: { source: 'homeboy_agent_tool_policy' },
      },
      runtime_env: {
        HOMEBOY_AGENT_TOOL_REQUEST_SCHEMA: 'homeboy/agent-tool-request/v1',
        HOMEBOY_AGENT_TOOL_RESULT_SCHEMA: 'homeboy/agent-tool-result/v1',
      },
      runtime_task: {
        ability: 'example/parent-tool',
        input: {},
      },
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: bridgeCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(bridgeResult.status, 0, bridgeResult.stderr || bridgeResult.stdout);
  const bridgeCaptured = readJson(bridgeCapturePath);
  const bridgePlugin = bridgeCaptured.input.extra_plugins.find((plugin) => plugin.slug === 'homeboy-runtime-tool-bridge');
  assert.equal(Boolean(bridgePlugin), true);
  assert.equal(bridgePlugin.loadAs, 'mu-plugin');
  assert.equal(bridgePlugin.pluginFile, 'homeboy-runtime-tool-bridge/homeboy-runtime-tool-bridge.php');
  assert.match(bridgeCaptured.input.runtime_env.HOMEBOY_AGENT_TOOL_BRIDGE_URL, /^http:\/\/127\.0\.0\.1:\d+$/);
  assert.equal(bridgeCaptured.input.runtime_env.HOMEBOY_AGENT_TASK_ID, 'agent-task-123');
  const bridgePluginSource = fs.readFileSync(path.join(bridgePlugin.source, 'homeboy-runtime-tool-bridge.php'), 'utf8');
  assert.match(bridgePluginSource, /Plugin Name: Homeboy Runtime Tool Bridge/);
  assert.match(bridgePluginSource, /add_filter\(\s*'datamachine_runtime_tool_result'/);
  assert.match(bridgePluginSource, new RegExp(bridgeCaptured.input.runtime_env.HOMEBOY_AGENT_TOOL_BRIDGE_URL.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));

  const abilityBridgeResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      runtime_task: {
        ability: 'example/validate-artifact',
        input: { artifact: { slug: 'example-site' } },
      },
      parent_request: {
        executor: {
          config: {
            ability: 'example/validate-artifact',
            runtime_output_projections: {
              import_validation_result: 'result.import_validation_result',
              finding_packets: 'result.finding_packets',
            },
          },
        },
      },
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: path.join(root, 'capture-ability-bridge.json'), OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(abilityBridgeResult.status, 0, abilityBridgeResult.stderr || abilityBridgeResult.stdout);
  const abilityBridgeOutput = JSON.parse(abilityBridgeResult.stdout);
  assert.equal(abilityBridgeOutput.success, true);
  assert.equal(abilityBridgeOutput.outputs.import_validation_result.status, 'passed');
  assert.equal(abilityBridgeOutput.outputs.finding_packets.count, 0);
  assert.equal(abilityBridgeOutput.outputs.typed_artifacts.import_validation_result.type, 'ImportValidationResult');
  assert.equal(abilityBridgeOutput.outputs.typed_artifacts.finding_packets.artifact_schema, 'example/finding-packets/v1');

  const missingRuntimeOutputResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      runtime_task: {
        ability: 'example/validate-artifact',
        input: { artifact: { slug: 'example-site' } },
      },
      parent_request: {
        executor: {
          config: {
            ability: 'example/validate-artifact',
            runtime_output_projections: {
              missing_validation_output: 'result.missing_validation_output',
            },
          },
        },
      },
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: path.join(root, 'capture-missing-runtime-output.json'), OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(missingRuntimeOutputResult.status, 1, missingRuntimeOutputResult.stderr || missingRuntimeOutputResult.stdout);
  const missingRuntimeOutput = JSON.parse(missingRuntimeOutputResult.stdout);
  assert.equal(missingRuntimeOutput.success, false);
  assert.equal(missingRuntimeOutput.diagnostics[0].class, 'runtime_task.outputs_missing');

  const missingRequiredArtifactResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      runtime_task: {
        ability: 'example/validate-artifact',
        input: { artifact: { slug: 'example-site' } },
      },
      artifact_declarations: [{
        schema: 'wp-codebox/artifact-declaration/v1',
        name: 'missing_required_report',
        type: 'MissingRequiredReport',
        artifact_schema: 'example/missing-required-report/v1',
        required: true,
      }],
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: path.join(root, 'capture-missing-required-artifact.json'), OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(missingRequiredArtifactResult.status, 1, missingRequiredArtifactResult.stderr || missingRequiredArtifactResult.stdout);
  const missingRequiredArtifactOutput = JSON.parse(missingRequiredArtifactResult.stdout);
  assert.equal(missingRequiredArtifactOutput.success, false);
  assert.equal(missingRequiredArtifactOutput.diagnostics[0].class, 'wp-codebox.required_typed_artifacts_missing');
  assert.equal(missingRequiredArtifactOutput.diagnostics[0].data.missing[0].name, 'missing_required_report');

  const failedRuntimeMetadataResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        source: 'bundles/example-agent',
        flow_slug: 'example-artifact-flow',
      },
      artifact_declarations: [{
        schema: 'wp-codebox/artifact-declaration/v1',
        name: 'concept_packet',
        type: 'ConceptPacket',
        artifact_schema: 'example/concept-packet/v1',
        required: true,
      }],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: path.join(root, 'capture-failed-runtime-metadata.json'),
      FIXTURE_WP_CODEBOX_FAILED_RUNTIME_METADATA: '1',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(failedRuntimeMetadataResult.status, 1, failedRuntimeMetadataResult.stderr || failedRuntimeMetadataResult.stdout);
  const failedRuntimeMetadataOutput = JSON.parse(failedRuntimeMetadataResult.stdout);
  assert.equal(failedRuntimeMetadataOutput.success, false);
  assert.equal(failedRuntimeMetadataOutput.diagnostics.some((diagnostic) => diagnostic.class === 'agent_runtime.failed'), true);
  assert.equal(failedRuntimeMetadataOutput.diagnostics.some((diagnostic) => diagnostic.class === 'wp-codebox.required_typed_artifacts_missing'), true);
  assert.equal(failedRuntimeMetadataOutput.session.status, 'failed');

  const codexCapturePath = path.join(root, 'capture-codex.json');
  const codexSecretEnv = [
    'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
    'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
    'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
  ];
  const codexResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
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
      AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '1',
      AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'account-id-value',
      AI_PROVIDER_OPENAI_CODEX_FEDRAMP: '0',
      HOMEBOY_WP_CODEBOX_CODEX_TOKEN_URL: codexOAuthServer.url,
    },
  });
  assert.equal(codexResult.status, 0, codexResult.stderr || codexResult.stdout);
  const codexCapture = readJson(codexCapturePath);
  const codexInput = codexCapture.input;
  assert.deepEqual(codexInput.secret_env, codexSecretEnv);
  assert.equal(codexInput.provider, 'codex');
  assert.equal(codexInput.model, 'gpt-5.5');
  assert.equal(codexInput.provider_plugin_paths[0], '/components/ai-provider-for-openai');
  assert.deepEqual(codexCapture.codex_env, {
    access_refreshed: true,
    refresh_refreshed: true,
    expires_refreshed: true,
  });
  assert(!JSON.stringify(codexInput).includes('access-token-value'));
  assert(!JSON.stringify(codexInput).includes('refresh-token-value'));

  const expiredCodexCapturePath = path.join(root, 'capture-expired-codex.json');
  const expiredCodexResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
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
      FIXTURE_WP_CODEBOX_CAPTURE: expiredCodexCapturePath,
      AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'expired-access-token-value',
      AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'expired-refresh-token-value',
      AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: 'not-a-time',
      AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'expired-account-id-value',
      AI_PROVIDER_OPENAI_CODEX_FEDRAMP: '0',
    },
  });
  assert.equal(expiredCodexResult.status, 1, expiredCodexResult.stderr || expiredCodexResult.stdout);
  assert.match(expiredCodexResult.stderr, /Codex provider auth preflight failed/);
  assert.match(expiredCodexResult.stderr, /AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT/);
  assert.match(expiredCodexResult.stderr, /Refresh Codex OAuth credentials/);
  assert(!expiredCodexResult.stderr.includes('expired-access-token-value'));
  assert(!expiredCodexResult.stderr.includes('expired-refresh-token-value'));
  assert(!fs.existsSync(expiredCodexCapturePath));

  const staleCodexCapturePath = path.join(root, 'capture-stale-codex.json');
  const staleCodexResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
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
      FIXTURE_WP_CODEBOX_CAPTURE: staleCodexCapturePath,
      AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'stale-access-token-value',
      AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'stale-refresh-token-value',
      AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '1',
      AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'stale-account-id-value',
      AI_PROVIDER_OPENAI_CODEX_FEDRAMP: '0',
      HOMEBOY_WP_CODEBOX_CODEX_TOKEN_URL: codexOAuthServer.url,
    },
  });
  assert.equal(staleCodexResult.status, 1, staleCodexResult.stderr || staleCodexResult.stdout);
  assert.match(staleCodexResult.stderr, /Codex provider auth preflight failed/);
  assert.match(staleCodexResult.stderr, /OAuth refresh returned HTTP 401/);
  assert.match(staleCodexResult.stderr, /Refresh Codex OAuth credentials/);
  assert(!staleCodexResult.stderr.includes('stale-access-token-value'));
  assert(!staleCodexResult.stderr.includes('stale-refresh-token-value'));
  assert(!fs.existsSync(staleCodexCapturePath));

  const implicitRuntimeRequest = { ...request };
  delete implicitRuntimeRequest.runtime_component_paths;
  const implicitRuntimeCapturePath = path.join(root, 'capture-implicit-runtime-stack.json');
  const implicitRuntimeResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
    '--mount', `${workspaceRoot}:/workspace/wp-coding-agents:readwrite`,
  ], {
    encoding: 'utf8',
    input: JSON.stringify(implicitRuntimeRequest),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: implicitRuntimeCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(implicitRuntimeResult.status, 0, implicitRuntimeResult.stderr || implicitRuntimeResult.stdout);
  const implicitRuntimeInput = readJson(implicitRuntimeCapturePath).input;
  assert.equal(implicitRuntimeInput.extra_plugins.find((plugin) => plugin.slug === 'agents-api').source, defaultAgentsApiPath);
  assert.equal(implicitRuntimeInput.extra_plugins.find((plugin) => plugin.slug === 'data-machine').source, defaultDataMachinePath);
  assert.equal(implicitRuntimeInput.extra_plugins.find((plugin) => plugin.slug === 'data-machine-code').source, defaultDataMachineCodePath);

  const labRuntimeRoot = path.join(root, 'lab-runtime-components');
  const labAgentsApi = path.join(labRuntimeRoot, 'agents-api');
  const labDataMachine = path.join(labRuntimeRoot, 'data-machine');
  const labDataMachineCode = path.join(labRuntimeRoot, 'data-machine-code');
  fs.mkdirSync(labAgentsApi, { recursive: true });
  fs.mkdirSync(labDataMachine, { recursive: true });
  fs.mkdirSync(labDataMachineCode, { recursive: true });
  fs.writeFileSync(path.join(labAgentsApi, 'agents-api.php'), "<?php\n/* Plugin Name: Agents API */\n");
  fs.writeFileSync(path.join(labDataMachine, 'data-machine.php'), "<?php\n/* Plugin Name: Data Machine */\n");
  fs.writeFileSync(path.join(labDataMachineCode, 'data-machine-code.php'), "<?php\n/* Plugin Name: Data Machine Code */\n");
  const labRuntimeCapturePath = path.join(root, 'capture-lab-runtime-components.json');
  const labRuntimeArtifacts = path.join(root, 'lab-runtime-artifacts');
  const labRuntimeResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
    '--artifacts', labRuntimeArtifacts,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      runtime_component_paths: {
        agents_api: '.ci/agents-api',
        agent_runtime: '.ci/data-machine',
        agent_runtime_tools: '.ci/data-machine-code',
      },
      component_contracts: [
        { slug: 'agents-api', path: '.ci/agents-api', loadAs: 'mu-plugin', activate: false },
      ],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: labRuntimeCapturePath,
      OPENCODE_API_KEY: 'redacted-test-key',
      HOMEBOY_LAB_OFFLOAD_JSON: JSON.stringify({
        workspace_mapping: {
          workspaces: [
            { role: 'dependency', local_path: '/Users/chubes/Developer/agents-api', remote_path: labAgentsApi },
            { role: 'dependency', local_path: '/Users/chubes/Developer/data-machine', remote_path: labDataMachine },
            { role: 'dependency', local_path: '/Users/chubes/Developer/data-machine-code', remote_path: labDataMachineCode },
          ],
        },
      }),
    },
  });
  assert.equal(labRuntimeResult.status, 0, labRuntimeResult.stderr || labRuntimeResult.stdout);
  const labRuntimeInput = readJson(labRuntimeCapturePath).input;
  const preparedLabDataMachine = path.join(labRuntimeArtifacts, 'prepared-plugins', 'data-machine');
  assert.equal(labRuntimeInput.runtime_component_paths.agent_runtime, preparedLabDataMachine);
  assert.equal(labRuntimeInput.extra_plugins.find((plugin) => plugin.slug === 'agents-api').source, path.join(labRuntimeArtifacts, 'prepared-plugins', 'agents-api'));
  assert.equal(labRuntimeInput.component_contracts.find((contract) => contract.slug === 'agents-api').path, path.join(labRuntimeArtifacts, 'prepared-plugins', 'agents-api'));
  assert.equal(labRuntimeInput.extra_plugins.find((plugin) => plugin.slug === 'data-machine-code').source, path.join(labRuntimeArtifacts, 'prepared-plugins', 'data-machine-code'));
  assert.equal(fs.existsSync(path.join(preparedLabDataMachine, 'data-machine.php')), true);

  const runtimeComponentSource = path.join(root, 'runtime-components', 'data-machine-code');
  const runtimeComponentArtifacts = path.join(root, 'prepared-runtime-component-artifacts');
  fs.mkdirSync(runtimeComponentSource, { recursive: true });
  fs.writeFileSync(path.join(runtimeComponentSource, 'data-machine-code.php'), "<?php\n/* Plugin Name: Data Machine Code */\n");
  const preparedRuntimeCapturePath = path.join(root, 'capture-prepared-runtime-component.json');
  const preparedRuntimeResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
    '--artifacts', runtimeComponentArtifacts,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      runtime_component_paths: {
        agent_runtime_tools: runtimeComponentSource,
      },
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: preparedRuntimeCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(preparedRuntimeResult.status, 0, preparedRuntimeResult.stderr || preparedRuntimeResult.stdout);
  const preparedRuntimeInput = readJson(preparedRuntimeCapturePath).input;
  const preparedRuntimePath = path.join(runtimeComponentArtifacts, 'prepared-plugins', 'data-machine-code');
  assert.equal(preparedRuntimeInput.extra_plugins.find((plugin) => plugin.slug === 'data-machine-code').source, preparedRuntimePath);
  assert.equal(preparedRuntimeInput.component_contracts.find((contract) => contract.slug === 'data-machine-code').path, preparedRuntimePath);
  assert.equal(preparedRuntimeInput.runtime_component_paths.agent_runtime_tools, preparedRuntimePath);
  assert.equal(fs.existsSync(path.join(preparedRuntimePath, 'data-machine-code.php')), true);

  const nestedRuntimeRoot = path.join(root, 'nested-runtime-components');
  const nestedDataMachine = path.join(nestedRuntimeRoot, 'data-machine');
  const nestedAgentsApi = path.join(nestedDataMachine, 'vendor', 'wordpress', 'agents-api');
  const nestedArtifacts = path.join(root, 'prepared-nested-runtime-component-artifacts');
  fs.mkdirSync(nestedAgentsApi, { recursive: true });
  fs.writeFileSync(path.join(nestedDataMachine, 'data-machine.php'), "<?php\n/* Plugin Name: Data Machine */\n");
  fs.writeFileSync(path.join(nestedAgentsApi, 'agents-api.php'), "<?php\n/* Plugin Name: Agents API */\n");
  const nestedRuntimeCapturePath = path.join(root, 'capture-nested-prepared-runtime-component.json');
  const nestedRuntimeResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
    '--artifacts', nestedArtifacts,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      runtime_component_paths: {
        agents_api: nestedAgentsApi,
        agent_runtime: nestedDataMachine,
      },
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: nestedRuntimeCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(nestedRuntimeResult.status, 0, nestedRuntimeResult.stderr || nestedRuntimeResult.stdout);
  const nestedRuntimeInput = readJson(nestedRuntimeCapturePath).input;
  const preparedDataMachine = path.join(nestedArtifacts, 'prepared-plugins', 'data-machine');
  const preparedAgentsApi = path.join(preparedDataMachine, 'vendor', 'wordpress', 'agents-api');
  assert.equal(nestedRuntimeInput.runtime_component_paths.agent_runtime, preparedDataMachine);
  assert.equal(nestedRuntimeInput.runtime_component_paths.agents_api, preparedAgentsApi);
  assert.equal(nestedRuntimeInput.extra_plugins.find((plugin) => plugin.slug === 'agents-api').source, preparedAgentsApi);
  assert.equal(nestedRuntimeInput.component_contracts.find((contract) => contract.slug === 'agents-api').path, preparedAgentsApi);
  assert.equal(fs.existsSync(path.join(preparedAgentsApi, 'agents-api.php')), true);

  const legacyRuntimeCapturePath = path.join(root, 'capture-legacy-runtime-stack.json');
  const legacyRuntimeResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      runtime_component_paths: undefined,
      agents_api_path: '/legacy/agents-api',
      data_machine_path: '/legacy/data-machine',
      data_machine_code_path: '/legacy/data-machine-code',
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: legacyRuntimeCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(legacyRuntimeResult.status, 0, legacyRuntimeResult.stderr || legacyRuntimeResult.stdout);
  const legacyRuntimeInput = readJson(legacyRuntimeCapturePath).input;
  assert.equal(legacyRuntimeInput.runtime_component_paths.agents_api, '/legacy/agents-api');
  assert.equal(legacyRuntimeInput.runtime_component_paths.agent_runtime, '/legacy/data-machine');
  assert.equal(legacyRuntimeInput.runtime_component_paths.agent_runtime_tools, '/legacy/data-machine-code');
  assert.equal(legacyRuntimeInput.component_contracts.find((contract) => contract.slug === 'agents-api').path, '/legacy/agents-api');
  assert.equal(Object.hasOwn(legacyRuntimeInput, 'data_machine_path'), false);
  assert.equal(Object.hasOwn(legacyRuntimeInput, 'data_machine_code_path'), false);

  const sourceRoot = path.join(root, 'source-plugin');
  fs.mkdirSync(sourceRoot, { recursive: true });
  const riskyArtifacts = path.join(sourceRoot, 'artifacts');
  const riskyResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
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

  const agentBundleCapturePath = path.join(root, 'capture-agent-bundle.json');
  const agentBundleResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'datamachine-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/example-repo/bundles/example-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.example_agent.issue_number' },
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: agentBundleCapturePath,
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(agentBundleResult.status, 0, agentBundleResult.stderr || agentBundleResult.stdout);
  const agentBundleCapture = readJson(agentBundleCapturePath);
  assert.equal(agentBundleCapture.argv[0], 'agent-task-run');
  assert(!agentBundleCapture.input.secret_env.some((name) => name.includes('HOMEBOY')));
  assert.equal(agentBundleCapture.input.sandbox_tool_policy.schema, 'wp-codebox/sandbox-tool-policy/v1');
  assert.equal(agentBundleCapture.input.sandbox_tool_policy.tools.length, 1);
  assert.equal(agentBundleCapture.input.sandbox_tool_policy.tools[0].id, 'homeboy/no-runtime-tools');
  assert.equal(agentBundleCapture.input.sandbox_tool_policy.tools[0].allowed, false);
  assert.equal(agentBundleCapture.input.sandbox_tool_policy.tools[0].runtime.environment, 'control_plane');
  assert.equal(agentBundleCapture.input.sandbox_tool_policy.tools[0].runtime.capability_scope, 'control_plane');
  assert.equal(agentBundleCapture.input.agent_bundle.engine_data_outputs.issue_number, 'metadata.engine_data.example_agent.issue_number');
  assert.equal(agentBundleCapture.input.runtime_task.ability, 'datamachine/run-agent-bundle');
  assert.equal(agentBundleCapture.input.runtime_task.input.source, '/workspace/example-repo/bundles/example-agent');
  assert.equal(agentBundleCapture.input.runtime_task.input.wait_for_completion, true);
  assert.equal(agentBundleCapture.input.runtime_task.input.runtime_bundles, undefined);
  const agentBundleOutput = JSON.parse(agentBundleResult.stdout);
  assert.equal(agentBundleOutput.success, true);
  assert.equal(agentBundleOutput.session.status, 'completed');
  assert.equal(agentBundleOutput.run.agentResult.scenarios[0].metadata.engine_data.example_agent.issue_number, 123);

  const singleResultCapturePath = path.join(root, 'capture-single-result-datamachine.json');
  const singleResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'single-result-datamachine-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/example-repo/bundles/example-agent',
        engine_data_outputs: {
          issue_number: 'metadata.engine_data.example_agent.issue_number',
          issue_url: 'metadata.engine_data.example_agent.issue_url',
        },
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: singleResultCapturePath,
      FIXTURE_WP_CODEBOX_SINGLE_RESULT: '1',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(singleResult.status, 0, singleResult.stderr || singleResult.stdout);
  const singleResultOutput = JSON.parse(singleResult.stdout);
  assert.equal(singleResultOutput.success, true);
  assert.equal(singleResultOutput.session.status, 'completed');
  assert.equal(singleResultOutput.run.agentResult.outputs.issue_number, 123);
  assert.equal(singleResultOutput.run.agentResult.outputs.issue_url, 'https://github.com/example-org/example-repo/issues/123');
  assert.equal(Array.isArray(singleResultOutput.run.agentResult.scenarios), false);
  assert.equal(singleResultOutput.diagnostics.some((diagnostic) => diagnostic.class === 'agent_runtime.output'), true);

  const canonicalBundleRunCapturePath = path.join(root, 'capture-canonical-datamachine-bundle-run.json');
  const canonicalBundleRunResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'canonical-datamachine-bundle-run-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/example-repo/bundles/example-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.example_agent.issue_number' },
        dry_run: true,
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: canonicalBundleRunCapturePath,
      FIXTURE_WP_CODEBOX_BUNDLE_RUN: '1',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(canonicalBundleRunResult.status, 0, canonicalBundleRunResult.stderr || canonicalBundleRunResult.stdout);
  const canonicalBundleRunOutput = JSON.parse(canonicalBundleRunResult.stdout);
  assert.equal(canonicalBundleRunOutput.success, true);
  assert.equal(canonicalBundleRunOutput.session.status, 'completed');
  assert.equal(canonicalBundleRunOutput.run.agentResult.scenarios[0].metadata.schema, 'datamachine/agent-bundle-run/v1');
  assert.equal(canonicalBundleRunOutput.run.agentResult.scenarios[0].metadata.job_status, 'completed');
  assert.equal(canonicalBundleRunOutput.run.agentResult.scenarios[0].metadata.engine_data.example_agent.issue_number, 123);
  assert.equal(canonicalBundleRunOutput.run.agentResult.scenarios[0].metadata.dry_run, true);
  assert.equal(canonicalBundleRunOutput.run.agentResult.scenarios[0].metrics.workflow_step_count, 1);
  assert.equal(canonicalBundleRunOutput.outputs.typed_artifacts.example_review.type, 'ExampleReviewArtifact');
  assert.equal(canonicalBundleRunOutput.outputs.typed_artifacts.example_review.artifact_schema, 'example/review-artifact/v1');
  assert.equal(canonicalBundleRunOutput.outputs.typed_artifacts.example_review.payload.review_ready, true);

  const recorderBundleRunCapturePath = path.join(root, 'capture-recorder-datamachine-bundle-run.json');
  const recorderBundleRunResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'recorder-datamachine-bundle-run-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/example-repo/bundles/example-agent',
        engine_data_outputs: {
          issue_number: 'metadata.engine_data.example_agent.issue_number',
          issue_url: 'metadata.engine_data.example_agent.issue_url',
        },
        tool_recorders: [{
          tool: 'github_issue_publish',
          record: {
            engine_key: 'example_agent',
            fields: {
              issue_number: 'data.issue_number',
              issue_url: 'data.issue_url',
            },
          },
        }],
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: recorderBundleRunCapturePath,
      FIXTURE_WP_CODEBOX_BUNDLE_RUN: '1',
      FIXTURE_WP_CODEBOX_BUNDLE_RUN_TOOL_RECORDERS: '1',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(recorderBundleRunResult.status, 0, recorderBundleRunResult.stderr || recorderBundleRunResult.stdout);
  const recorderBundleRunOutput = JSON.parse(recorderBundleRunResult.stdout);
  assert.equal(recorderBundleRunOutput.success, true);
  assert.equal(recorderBundleRunOutput.status, 'completed');
  assert.equal(recorderBundleRunOutput.outputs.issue_number, 123);
  assert.equal(recorderBundleRunOutput.outputs.issue_url, 'https://github.com/example-org/example-repo/issues/123');
  assert.equal(recorderBundleRunOutput.run.agentResult.outputs.issue_number, 123);
  assert.equal(recorderBundleRunOutput.run.agentResult.outputs.issue_url, 'https://github.com/example-org/example-repo/issues/123');

  const projectionBundleRunCapturePath = path.join(root, 'capture-projection-bundle-run.json');
  const projectionBundleRunResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'projection-bundle-run-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/example-repo/bundles/example-agent',
        runtime_output_projections: {
          issue_number: 'metadata.engine_data.example_agent.issue_number',
          issue_url: 'metadata.engine_data.example_agent.issue_url',
        },
        evidence_projections: [{
          operation: 'github_issue_publish',
          outputs: {
            issue_number: 'data.issue_number',
            issue_url: 'data.issue_url',
          },
        }],
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: projectionBundleRunCapturePath,
      FIXTURE_WP_CODEBOX_BUNDLE_RUN: '1',
      FIXTURE_WP_CODEBOX_BUNDLE_RUN_TOOL_RECORDERS: '1',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(projectionBundleRunResult.status, 0, projectionBundleRunResult.stderr || projectionBundleRunResult.stdout);
  const projectionBundleRunOutput = JSON.parse(projectionBundleRunResult.stdout);
  assert.equal(projectionBundleRunOutput.success, true);
  assert.equal(projectionBundleRunOutput.outputs.issue_number, 123);
  assert.equal(projectionBundleRunOutput.outputs.issue_url, 'https://github.com/example-org/example-repo/issues/123');

  const completedBundleNonzeroExitCapturePath = path.join(root, 'capture-completed-bundle-nonzero-exit.json');
  const completedBundleNonzeroExitResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'completed-bundle-nonzero-exit-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/example-repo/bundles/example-agent',
        engine_data_outputs: {
          issue_number: 'metadata.engine_data.example_agent.issue_number',
          issue_url: 'metadata.engine_data.example_agent.issue_url',
        },
        tool_recorders: [{
          tool: 'github_issue_publish',
          record: {
            engine_key: 'example_agent',
            fields: {
              issue_number: 'data.issue_number',
              issue_url: 'data.issue_url',
            },
          },
        }],
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: completedBundleNonzeroExitCapturePath,
      FIXTURE_WP_CODEBOX_BUNDLE_RUN: '1',
      FIXTURE_WP_CODEBOX_BUNDLE_RUN_TOOL_RECORDERS: '1',
      FIXTURE_WP_CODEBOX_EXIT_CODE: '1',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(completedBundleNonzeroExitResult.status, 0, completedBundleNonzeroExitResult.stderr || completedBundleNonzeroExitResult.stdout);
  const completedBundleNonzeroExitOutput = JSON.parse(completedBundleNonzeroExitResult.stdout);
  assert.equal(completedBundleNonzeroExitOutput.success, true);
  assert.equal(completedBundleNonzeroExitOutput.status, 'completed');
  assert.equal(completedBundleNonzeroExitOutput.outputs.issue_number, 123);
  assert.equal(completedBundleNonzeroExitOutput.session.status, 'completed');

  const incompleteDatamachineCapturePath = path.join(root, 'capture-incomplete-datamachine.json');
  const incompleteDatamachineResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'incomplete-datamachine-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/example-repo/bundles/example-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.example_agent.issue_number' },
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: incompleteDatamachineCapturePath,
      FIXTURE_WP_CODEBOX_INCOMPLETE_AGENT_BUNDLE: '1',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(incompleteDatamachineResult.status, 1, incompleteDatamachineResult.stderr || incompleteDatamachineResult.stdout);
  const incompleteDatamachineOutput = JSON.parse(incompleteDatamachineResult.stdout);
  assert.equal(incompleteDatamachineOutput.success, false);
  assert.equal(incompleteDatamachineOutput.session.status, 'failed');
  assert.equal(incompleteDatamachineOutput.diagnostics[0].class, 'agent_runtime.workload.incomplete');
  assert.equal(incompleteDatamachineOutput.diagnostics[0].data.reason, 'missing_runtime_output_projections');
  assert.match(incompleteDatamachineOutput.diagnostics[0].message, /issue_number/);

  const failedDatamachineCapturePath = path.join(root, 'capture-failed-datamachine.json');
  const failedDatamachineResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'failed-datamachine-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      execution_kind: 'agent_bundle',
      homeboy_extensions: path.join(__dirname, '..'),
      agent_bundle: {
        bundle_path: '/workspace/example-repo/bundles/example-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.example_agent.issue_number' },
      },
      provider_plugin_paths: [],
    }),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: failedDatamachineCapturePath,
      FIXTURE_WP_CODEBOX_FAILED_AGENT_BUNDLE: '1',
      OPENCODE_API_KEY: 'redacted-test-key',
    },
  });
  assert.equal(failedDatamachineResult.status, 1, failedDatamachineResult.stderr || failedDatamachineResult.stdout);
  const failedDatamachineOutput = JSON.parse(failedDatamachineResult.stdout);
  assert.equal(failedDatamachineOutput.success, false);
  assert.equal(failedDatamachineOutput.diagnostics[0].class, 'agent_runtime.workload.failed');
  assert.equal(failedDatamachineOutput.diagnostics[0].data.reason, 'scenario_error');
  assert.match(failedDatamachineOutput.summary, /did not reach a terminal state/);

  const nonExecutableCapturePath = path.join(root, 'capture-non-executable.json');
  const nonExecutableFixture = createFixtureWpCodebox(root, 0o644);
  const nonExecutableResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', nonExecutableFixture,
    '--agents-api', '/components/agents-api',
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: nonExecutableCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(nonExecutableResult.status, 0, nonExecutableResult.stderr || nonExecutableResult.stdout);
  const nonExecutableCapture = readJson(nonExecutableCapturePath);
  assert.equal(nonExecutableCapture.argv[0], 'agent-task-run');
  assert.equal(pathInside(root, nonExecutableCapture.input.artifacts_path), false);

  const validationArtifacts = path.join(root, 'validation-failure-artifacts');
  const validationCapturePath = path.join(root, 'capture-validation-failure.json');
  const validationFailureResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
    '--artifacts', validationArtifacts,
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: validationCapturePath,
      FIXTURE_WP_CODEBOX_VALIDATION_FAILURE: '1',
      OPENCODE_API_KEY: 'super-secret-validation-key',
    },
  });
  assert.equal(validationFailureResult.status, 1, validationFailureResult.stderr || validationFailureResult.stdout);
  const validationOutput = JSON.parse(validationFailureResult.stdout);
  assert.equal(validationOutput.success, false);
  assert.equal(validationOutput.diagnostics[0].class, 'wp-codebox.agent_task_run_failed');
  assert.match(validationOutput.diagnostics[0].message, /RecipeValidationError/);
  assert.equal(validationOutput.diagnostics.some((diagnostic) => diagnostic.class === 'wp-codebox.command.evidence_preserved'), true);
  const evidence = readJson(path.join(validationArtifacts, 'wp-codebox-command-evidence.json'));
  assert.equal(fs.existsSync(evidence.stderr_path), true);
  assert.equal(fs.existsSync(evidence.input_evidence_path), true);
  assert.equal(evidence.copied_generated_paths.length, 1);
  assert.match(fs.readFileSync(evidence.stderr_path, 'utf8'), /workflow\.steps\[0\]\.command is required/);
  assert(!fs.readFileSync(evidence.input_evidence_path, 'utf8').includes('super-secret-validation-key'));
  assert(!fs.readFileSync(evidence.copied_generated_paths[0].path, 'utf8').includes('super-secret-validation-key'));

  const jsonValidationArtifacts = path.join(root, 'json-validation-failure-artifacts');
  const jsonValidationCapturePath = path.join(root, 'capture-json-validation-failure.json');
  const jsonValidationFailureResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
    '--wp-codebox-bin', fixtureWpCodebox,
    '--artifacts', jsonValidationArtifacts,
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: {
      ...process.env,
      FIXTURE_WP_CODEBOX_CAPTURE: jsonValidationCapturePath,
      FIXTURE_WP_CODEBOX_JSON_VALIDATION_FAILURE: '1',
      OPENCODE_API_KEY: 'super-secret-json-validation-key',
    },
  });
  assert.equal(jsonValidationFailureResult.status, 1, jsonValidationFailureResult.stderr || jsonValidationFailureResult.stdout);
  const jsonValidationOutput = JSON.parse(jsonValidationFailureResult.stdout);
  assert.equal(jsonValidationOutput.success, false);
  assert.equal(jsonValidationOutput.diagnostics.some((diagnostic) => diagnostic.class === 'wp-codebox.command.evidence_preserved'), true);
  const jsonEvidence = readJson(path.join(jsonValidationArtifacts, 'wp-codebox-command-evidence.json'));
  assert.equal(fs.existsSync(jsonEvidence.stdout_path), true);
  assert.equal(fs.existsSync(jsonEvidence.input_evidence_path), true);
  assert.equal(jsonEvidence.copied_generated_paths.length, 1);
  assert.match(fs.readFileSync(jsonEvidence.stdout_path, 'utf8'), /RecipeValidationError/);
  assert(!fs.readFileSync(jsonEvidence.stdout_path, 'utf8').includes('super-secret-json-validation-key'));
  assert(!fs.readFileSync(jsonEvidence.copied_generated_paths[0].path, 'utf8').includes('super-secret-json-validation-key'));

  const missingSecretResult = spawnSync(process.execPath, [
    wpCodeboxTaskRunner,
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
  if (codexOAuthServer) {
    codexOAuthServer.stop();
  }
  fs.rmSync(root, { recursive: true, force: true });
}
