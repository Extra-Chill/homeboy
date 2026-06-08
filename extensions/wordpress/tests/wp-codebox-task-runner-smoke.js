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
const isAgentBundle = Boolean(input.agent_bundle && Object.keys(input.agent_bundle).length);
const bundleRun = isAgentBundle && process.env.FIXTURE_WP_CODEBOX_BUNDLE_RUN
  ? {
      schema: 'datamachine/agent-bundle-run/v1',
      success: true,
      dry_run: true,
      job_id: 1,
      job_status: 'completed',
      bundle: {
        bundle_slug: 'store-idea-agent',
        flow_slug: 'static-site-manual-flow',
        pipeline_slug: 'static-site-pipeline',
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
                      issue_url: 'https://github.com/chubes4/wp-site-generator/issues/123'
                    }
                  }
                }
              }]
            }
          }
        : { store_idea_agent: { issue_number: 123, issue_url: 'https://github.com/chubes4/wp-site-generator/issues/123' } }
    }
  : null;
const agentResult = isAgentBundle && process.env.FIXTURE_WP_CODEBOX_FAILED_AGENT_BUNDLE
  ? { scenarios: [{ id: 'agent-bundle', metadata: { error: 'Agent bundle child job 456 did not reach a terminal state after drain; current status is pending.' } }] }
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
            issue_url: 'https://github.com/chubes4/wp-site-generator/issues/123'
          },
          diagnostics: [{ class: 'agent_runtime.output', message: 'Semantic outputs captured.' }]
        }
  : (isAgentBundle && !process.env.FIXTURE_WP_CODEBOX_INCOMPLETE_AGENT_BUNDLE
      ? { metrics: { config_present: 1 }, metadata: { engine_data: { store_idea_agent: { issue_number: 123 } } } }
      : { status: 'completed' })));
fs.writeFileSync(out, JSON.stringify({ argv: process.argv.slice(2), input }, null, 2));
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
    runtime_component_paths: {
      agent_runtime: '/components/data-machine',
      agent_runtime_tools: '/components/data-machine-code',
    },
    runtime_stack_mounts: [{ source: '/components/php-ai-client', target: '/wordpress/wp-includes/php-ai-client', mode: 'readonly' }],
    runtime_overlays: [{ kind: 'bundled-library', library: 'php-ai-client' }],
    secret_env: ['OPENCODE_API_KEY'],
    orchestrator: { agent_task_id: 'agent-task-123', run_id: 'run-123' },
  };

  const result = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
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
  assert.equal(captured.input.sandbox_tool_policy.schema, 'wp-codebox/sandbox-tool-policy/v1');
  assert.equal(captured.input.sandbox_tool_policy.version, 1);
  assert.equal(captured.input.sandbox_tool_policy.tools[0].id, 'homeboy/no-runtime-tools');
  assert.equal(captured.input.sandbox_tool_policy.tools[0].allowed, false);
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
  assert.equal(captured.input.runtime_component_paths.agent_runtime, '/components/data-machine');
  assert.equal(captured.input.runtime_component_paths.agent_runtime_tools, '/components/data-machine-code');
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'agents-api').source, '/components/agents-api');
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'data-machine').source, '/components/data-machine');
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'data-machine-code').source, '/components/data-machine-code');
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'data-machine').loadAs, 'mu-plugin');
  assert.equal(captured.input.extra_plugins.find((plugin) => plugin.slug === 'data-machine-code').activate, false);
  // WP Codebox 0.8.0 mounts runtime components from `component_contracts`
  // ({ slug, path }), not the legacy `runtime_component_paths` fields.
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'agents-api').path, '/components/agents-api');
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'data-machine').path, '/components/data-machine');
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'data-machine-code').path, '/components/data-machine-code');
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'data-machine').loadAs, 'mu-plugin');
  assert.equal(captured.input.component_contracts.find((contract) => contract.slug === 'data-machine-code').activate, false);
  assert(!JSON.stringify(captured.input).includes('redacted-test-key'));

  const runtimeTaskCapturePath = path.join(root, 'capture-runtime-task.json');
  const runtimeTaskResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
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
      workspaces: [{ target: '/workspace/codebox-canary', mode: 'readwrite' }],
    }),
    env: { ...process.env, FIXTURE_WP_CODEBOX_CAPTURE: runtimeTaskCapturePath, OPENCODE_API_KEY: 'redacted-test-key' },
  });
  assert.equal(runtimeTaskResult.status, 0, runtimeTaskResult.stderr || runtimeTaskResult.stdout);
  const runtimeTaskCaptured = readJson(runtimeTaskCapturePath);
  assert.equal(runtimeTaskCaptured.input.sandbox_tool_policy.tools[0].id, 'homeboy-canary/write-file');
  assert.equal(runtimeTaskCaptured.input.runtime_task.ability, 'homeboy-canary/write-file');
  assert.equal(runtimeTaskCaptured.input.workspaces[0].target, '/workspace/codebox-canary');

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

  const agentBundleCapturePath = path.join(root, 'capture-agent-bundle.json');
  const agentBundleResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'datamachine-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/wp-site-generator/bundles/store-idea-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.store_idea_agent.issue_number' },
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
  assert.equal(agentBundleCapture.input.agent_bundle.engine_data_outputs.issue_number, 'metadata.engine_data.store_idea_agent.issue_number');
  assert.equal(agentBundleCapture.input.runtime_task.ability, 'datamachine/run-agent-bundle');
  assert.equal(agentBundleCapture.input.runtime_task.input.source, '/workspace/wp-site-generator/bundles/store-idea-agent');
  assert.equal(agentBundleCapture.input.runtime_task.input.wait_for_completion, true);
  assert.equal(agentBundleCapture.input.runtime_task.input.runtime_bundles, undefined);
  const agentBundleOutput = JSON.parse(agentBundleResult.stdout);
  assert.equal(agentBundleOutput.success, true);
  assert.equal(agentBundleOutput.session.status, 'completed');
  assert.equal(agentBundleOutput.run.agentResult.scenarios[0].metadata.engine_data.store_idea_agent.issue_number, 123);

  const singleResultCapturePath = path.join(root, 'capture-single-result-datamachine.json');
  const singleResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'single-result-datamachine-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/wp-site-generator/bundles/store-idea-agent',
        engine_data_outputs: {
          issue_number: 'metadata.engine_data.store_idea_agent.issue_number',
          issue_url: 'metadata.engine_data.store_idea_agent.issue_url',
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
  assert.equal(singleResultOutput.run.agentResult.outputs.issue_url, 'https://github.com/chubes4/wp-site-generator/issues/123');
  assert.equal(Array.isArray(singleResultOutput.run.agentResult.scenarios), false);
  assert.equal(singleResultOutput.diagnostics.some((diagnostic) => diagnostic.class === 'agent_runtime.output'), true);

  const canonicalBundleRunCapturePath = path.join(root, 'capture-canonical-datamachine-bundle-run.json');
  const canonicalBundleRunResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'canonical-datamachine-bundle-run-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/wp-site-generator/bundles/store-idea-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.store_idea_agent.issue_number' },
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
  assert.equal(canonicalBundleRunOutput.run.agentResult.scenarios[0].metadata.engine_data.store_idea_agent.issue_number, 123);
  assert.equal(canonicalBundleRunOutput.run.agentResult.scenarios[0].metadata.dry_run, true);
  assert.equal(canonicalBundleRunOutput.run.agentResult.scenarios[0].metrics.workflow_step_count, 1);

  const recorderBundleRunCapturePath = path.join(root, 'capture-recorder-datamachine-bundle-run.json');
  const recorderBundleRunResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'recorder-datamachine-bundle-run-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/wp-site-generator/bundles/store-idea-agent',
        engine_data_outputs: {
          issue_number: 'metadata.engine_data.store_idea_agent.issue_number',
          issue_url: 'metadata.engine_data.store_idea_agent.issue_url',
        },
        tool_recorders: [{
          tool: 'github_issue_publish',
          record: {
            engine_key: 'store_idea_agent',
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
  assert.equal(recorderBundleRunOutput.outputs.issue_url, 'https://github.com/chubes4/wp-site-generator/issues/123');
  assert.equal(recorderBundleRunOutput.run.agentResult.outputs.issue_number, 123);
  assert.equal(recorderBundleRunOutput.run.agentResult.outputs.issue_url, 'https://github.com/chubes4/wp-site-generator/issues/123');

  const completedBundleNonzeroExitCapturePath = path.join(root, 'capture-completed-bundle-nonzero-exit.json');
  const completedBundleNonzeroExitResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'completed-bundle-nonzero-exit-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/wp-site-generator/bundles/store-idea-agent',
        engine_data_outputs: {
          issue_number: 'metadata.engine_data.store_idea_agent.issue_number',
          issue_url: 'metadata.engine_data.store_idea_agent.issue_url',
        },
        tool_recorders: [{
          tool: 'github_issue_publish',
          record: {
            engine_key: 'store_idea_agent',
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
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
    '--wp-codebox-bin',
    fixtureWpCodebox,
    '--artifacts',
    path.join(root, 'incomplete-datamachine-artifacts'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...request,
      agent_bundle: {
        bundle_path: '/workspace/wp-site-generator/bundles/store-idea-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.store_idea_agent.issue_number' },
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
  assert.equal(incompleteDatamachineOutput.diagnostics[0].data.reason, 'missing_engine_data_outputs');
  assert.match(incompleteDatamachineOutput.diagnostics[0].message, /issue_number/);

  const failedDatamachineCapturePath = path.join(root, 'capture-failed-datamachine.json');
  const failedDatamachineResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
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
        bundle_path: '/workspace/wp-site-generator/bundles/store-idea-agent',
        engine_data_outputs: { issue_number: 'metadata.engine_data.store_idea_agent.issue_number' },
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
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'),
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
