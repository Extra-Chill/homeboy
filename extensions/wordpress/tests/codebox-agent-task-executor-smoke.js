'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const {
  agentTaskOutcomeFromCodeboxResult,
  codeboxTaskRequestFromAgentTaskRequest,
  providerContract,
} = require('../lib/codebox-agent-task-executor');

const fixtureCodeboxCoreModule = path.join(__dirname, 'fixtures', 'wp-codebox-core-agent-task-normalizer.mjs');
const codexSecretEnv = [
  'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
  'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
  'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
];
const claudeCodeRefreshTokenEnv = 'AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN';

function fixtureEnv(overrides = {}) {
  const env = { ...process.env, ...overrides };
  if (!Object.hasOwn(overrides, 'HOMEBOY_WP_CODEBOX_CORE_MODULE')) {
    delete env.HOMEBOY_WP_CODEBOX_CORE_MODULE;
  }
  return env;
}

function writeFixtureTaskRunner(root) {
  const fixture = path.join(root, 'fixture-task-runner.cjs');
  const capture = path.join(root, 'capture.json');
  fs.writeFileSync(fixture, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
  const request = JSON.parse(fs.readFileSync(0, 'utf8'));
const codexSecretEnv = ${JSON.stringify(codexSecretEnv)};
fs.writeFileSync(${JSON.stringify(capture)}, JSON.stringify({
  argv: process.argv.slice(2),
  request,
  env_presence: Object.fromEntries(codexSecretEnv.map((name) => [name, Boolean(process.env[name])])),
}, null, 2));
process.stdout.write(JSON.stringify({
  success: true,
  status: 'completed',
  summary: 'Sandbox completed.',
  artifacts: [{ id: 'artifact-1', kind: 'screenshot', path: '/artifacts/screenshot.png' }],
  evidence_refs: [{ kind: 'preview', uri: 'https://example.test/preview', label: 'Preview' }],
  run: {
    runId: 'fixture-run-1',
    status: 'succeeded',
    runtime: { id: 'fixture-runtime-1', status: 'destroyed' },
    agentResult: {
      changedFiles: { count: 2 },
      patch: { bytes: 123, sha256: 'fixture-patch-sha' }
    }
  },
  recipe_run: {
    pack: 'fixture-recipes',
    name: 'fixture-recipe',
    probe: { success: true }
  },
  metadata: { run_id: 'codebox-run-1' }
}));
`);
  fs.chmodSync(fixture, 0o755);
  return { fixture, capture };
}

function writeHangingTaskRunner(root) {
  const fixture = path.join(root, 'hanging-task-runner.cjs');
  fs.writeFileSync(fixture, `#!/usr/bin/env node
'use strict';
setInterval(() => {}, 1000);
`);
  fs.chmodSync(fixture, 0o755);
  return fixture;
}

function writeMissingSecretTaskRunner(root) {
  const fixture = path.join(root, 'missing-secret-task-runner.cjs');
  fs.writeFileSync(fixture, `#!/usr/bin/env node
'use strict';
console.error('Required WP Codebox secret environment variable missing: AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN, AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN');
process.exit(1);
`);
  fs.chmodSync(fixture, 0o755);
  return fixture;
}

function writeEmptyJsonTaskRunner(root) {
  const fixture = path.join(root, 'empty-json-task-runner.cjs');
  fs.writeFileSync(fixture, `#!/usr/bin/env node
'use strict';
process.stdout.write('{}');
process.exit(0);
`);
  fs.chmodSync(fixture, 0o755);
  return fixture;
}

function writeEmptyStdoutTaskRunner(root) {
  const fixture = path.join(root, 'empty-stdout-task-runner.cjs');
  fs.writeFileSync(fixture, `#!/usr/bin/env node
'use strict';
process.exit(0);
`);
  fs.chmodSync(fixture, 0o755);
  return fixture;
}

function writeFakeWpCodebox(root) {
  const fixture = path.join(root, 'fake-wp-cli.cjs');
  const capture = path.join(root, 'fake-wp-cli-capture.json');
  fs.writeFileSync(fixture, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const inputArg = process.argv.find((arg) => arg.startsWith('--input-file='));
const inputPath = inputArg ? inputArg.slice('--input-file='.length) : process.argv[process.argv.indexOf('--input-file') + 1];
const input = JSON.parse(fs.readFileSync(inputPath, 'utf8'));
fs.writeFileSync(${JSON.stringify(capture)}, JSON.stringify({ argv: process.argv.slice(2), input }, null, 2));
if (process.env.FIXTURE_WP_CODEBOX_AGENT_TASK_FAILURE) {
  process.stdout.write(JSON.stringify({
    success: false,
    schema: 'wp-codebox/agent-task-run/v1',
    status: 'failed',
    summary: 'WP Codebox agent task failed.',
    session: { id: input.sandbox_session_id, status: 'failed' },
    artifacts: input.artifacts_path,
    metadata: {}
  }));
  process.exit(0);
}
process.stdout.write(JSON.stringify({
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  session: {
    schema: 'wp-codebox/sandbox-session/v1',
    id: input.sandbox_session_id,
    status: 'completed',
    artifacts: { bundle_id: 'fake-artifact-bundle', path: input.artifacts_path, preview_url: 'https://preview.example.test/fake' },
    orchestrator: input.orchestrator
  },
  task_input: input,
  artifacts: input.artifacts_path,
  agent_result: {
    scenarios: [{
      id: 'agent-bundle',
      metadata: {
        transcript_artifacts: { json: input.artifacts_path + '/transcript.json' },
        replay_bundle_path: input.artifacts_path + '/replay-bundle',
        engine_data: { static_site_agent: { pr_url: 'https://github.com/chubes4/wp-site-generator/pull/123' } }
      }
    }]
  },
  metadata: {
    agent_runtime: {
      bundle: input.agent_bundle,
      workload: {
        scenarios: [{
          id: 'agent-bundle',
          metadata: {
            transcript_artifacts: { json: input.artifacts_path + '/transcript.json' },
            replay_bundle_path: input.artifacts_path + '/replay-bundle',
            engine_data: { static_site_agent: { pr_url: 'https://github.com/chubes4/wp-site-generator/pull/123' } }
          }
        }]
      }
    }
  }
}));
`);
  fs.chmodSync(fixture, 0o755);
  return { fixture, capture };
}

function writeBundleFixture(root) {
  const bundle = path.join(root, 'static-site-agent');
  fs.mkdirSync(bundle, { recursive: true });
  fs.writeFileSync(path.join(bundle, 'manifest.json'), '{}\n');
  return bundle;
}

function writeTimeoutArtifacts(artifactRoot, taskId) {
  const bundleRoot = path.join(artifactRoot, `artifact-${taskId}`);
  const filesRoot = path.join(bundleRoot, 'files');
  fs.mkdirSync(filesRoot, { recursive: true });
  fs.writeFileSync(path.join(bundleRoot, 'manifest.json'), JSON.stringify({
    schema: 'wp-codebox/artifact-manifest/v1',
    phase: 'agent.inspecting-runtime',
  }));
  fs.writeFileSync(path.join(filesRoot, 'runtime-reference-manifest.json'), JSON.stringify({
    schema: 'wp-codebox/runtime-reference-manifest/v1',
    runtime: { id: `runtime-${taskId}` },
  }));
  fs.writeFileSync(path.join(filesRoot, 'command.log'), 'ran wp-codebox.agent-sandbox-run\n');
  fs.writeFileSync(path.join(filesRoot, 'agent-transcript.jsonl'), '{"role":"assistant","content":"partial transcript"}\n');
  fs.writeFileSync(path.join(filesRoot, 'heartbeat.json'), JSON.stringify({
    phase: 'agent.inspecting-runtime',
    heartbeat: { at: '2026-06-01T00:00:00.000Z', turn: 3 },
  }));
  return bundleRoot;
}

const request = {
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'task-123',
  group_key: 'visual-evidence',
  executor: {
    backend: 'codebox',
    model: 'gpt-5.5',
    config: {
      provider: 'openai',
      provider_plugin_paths: ['/providers/openai'],
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
      secret_env: ['OPENAI_API_KEY'],
      max_turns: 8,
    },
  },
  instructions: 'Inspect the WordPress runtime and capture evidence.',
  inputs: {
    title: 'Capture WordPress visual evidence',
    audit_findings: [{ id: 'finding-1' }],
    orchestrator: { run_id: 'run-123' },
  },
  source_refs: [{ kind: 'issue', uri: 'https://github.com/Extra-Chill/homeboy-extensions/issues/966' }],
  workspace: { mode: 'ephemeral' },
  policy: { read: 'sandbox', write: 'sandbox', apply: 'review' },
  limits: { timeout_ms: 120000 },
  expected_artifacts: ['screenshot'],
};

const provider = providerContract();
assert.equal(provider.backend, 'codebox');
assert.equal(provider.request_schema, 'homeboy/agent-task-request/v1');
assert.equal(provider.outcome_schema, 'homeboy/agent-task-outcome/v1');
assert.deepEqual(provider.request_required_fields, ['schema', 'task_id', 'executor.backend', 'instructions']);
assert.deepEqual(provider.outcome_statuses, ['succeeded', 'failed', 'no_op', 'unable_to_remediate', 'timeout', 'provider_error']);
assert.deepEqual(provider.failure_classifications, ['provider', 'timeout', 'execution_failed']);
assert.deepEqual(provider.redacted_metadata_keys, ['secret_env_values', 'secretEnvValues', 'secrets']);
assert.deepEqual(provider.workspace_materialization, { cwd: 'git_checkout' });
assert.equal(provider.status, 'active');
assert.equal(provider.integration_contract, 'wp-codebox-cli/agent-task-run');
assert.equal(provider.capabilities.includes('browser_runtime'), true);
assert.equal(provider.capabilities.includes('workspace_tools'), true);
assert.equal(provider.capabilities.includes('patch_artifacts'), true);
assert.equal(provider.capabilities.includes('cleanup_observability'), true);
assert.equal(provider.capabilities.includes('agent_bundle_execution'), true);
assert.equal(provider.capabilities.includes('typed_bundle_outputs'), true);
assert.equal(provider.capabilities.includes('external_recipe_packs'), true);
assert.equal(provider.capabilities.includes('recipe_probe_artifacts'), true);
assert.deepEqual(provider.runtime_gap_trackers, []);

const codeboxRequest = codeboxTaskRequestFromAgentTaskRequest(request);
assert.equal(codeboxRequest.schema, 'wp-codebox/task-input/v1');
assert.equal(codeboxRequest.goal, request.instructions);
assert.equal(codeboxRequest.sandbox_session_id, 'task-123');
assert.equal(codeboxRequest.provider, 'openai');
assert.equal(codeboxRequest.model, 'gpt-5.5');
assert.deepEqual(codeboxRequest.provider_plugin_paths, ['/providers/openai']);
assert.deepEqual(codeboxRequest.runtime_stack_mounts, [{
  type: 'directory',
  source: '/components/php-ai-client',
  target: '/wordpress/wp-includes/php-ai-client',
  mode: 'readonly',
  metadata: { component: 'php-ai-client', ref: 'custom-provider-auth' },
}]);
assert.deepEqual(codeboxRequest.runtime_overlays, [{
  type: 'bundled-library',
  library: 'php-ai-client',
  source: '/components/php-ai-client',
  target: '/wordpress/wp-includes/php-ai-client',
  metadata: { component: 'php-ai-client', ref: 'custom-provider-auth' },
}]);
assert.deepEqual(codeboxRequest.secret_env, ['OPENAI_API_KEY']);
assert.equal(codeboxRequest.max_turns, 8);
assert.equal(codeboxRequest.task_timeout_seconds, 120);
assert.equal(codeboxRequest.expected_artifacts[0], 'screenshot');
assert.equal(codeboxRequest.orchestrator.agent_task_id, 'task-123');
assert.equal(codeboxRequest.context.audit_findings[0].id, 'finding-1');
assert.deepEqual(codeboxRequest.agent_bundle, {});

const executorSecretEnvRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'executor-secret-env-task-123',
  executor: {
    backend: 'codebox',
    model: 'claude-sonnet-4-6',
    secret_env: [
      'AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN',
      'AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN',
      'AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT',
    ],
    config: {
      provider: 'claude-code',
      wp: '7.0',
      provider_plugin_paths: ['/providers/claude-code'],
    },
  },
});
assert.deepEqual(executorSecretEnvRequest.secret_env, [
  'AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN',
  'AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN',
  'AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT',
]);
assert.equal(executorSecretEnvRequest.wp, '7.0');

const claudeCodeDefaultSecretEnvRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'claude-code-default-secret-env-task-123',
  executor: {
    backend: 'codebox',
    model: 'claude-sonnet-4-6',
    config: {
      provider: 'claude-code',
      provider_plugin_paths: ['/providers/claude-code'],
    },
  },
});
assert.deepEqual(claudeCodeDefaultSecretEnvRequest.secret_env, [claudeCodeRefreshTokenEnv]);

const runtimeTaskRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'runtime-task-123',
  executor: {
    backend: 'codebox',
    config: {
      sandbox_tool_policy: {
        schema: 'wp-codebox/sandbox-tool-policy/v1',
        version: 1,
        tools: [{ id: 'homeboy-canary/write-file', allowed: true }],
      },
    },
  },
  inputs: {
    runtime_task: {
      ability: 'homeboy-canary/write-file',
      input: { path: '/workspace/codebox-canary/CANARY.md', content: 'after\n' },
    },
    workspaces: [{ target: '/workspace/codebox-canary', mode: 'readwrite' }],
  },
});
assert.equal(runtimeTaskRequest.sandbox_tool_policy.tools[0].id, 'homeboy-canary/write-file');
assert.equal(runtimeTaskRequest.runtime_task.ability, 'homeboy-canary/write-file');
assert.equal(runtimeTaskRequest.workspaces[0].target, '/workspace/codebox-canary');

const abilityBridgeRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'ability-bridge-task-123',
  executor: {
    backend: 'codebox',
    config: {
      execution_kind: 'wp_codebox_ability',
      ability: 'example/validate-artifact',
      ability_input: { artifact: { slug: 'example-site' }, report: '/artifacts/import-report.json' },
      output_mappings: {
        validation_result: 'result.import_validation_result',
      },
      engine_data_outputs: {
        validation_result: 'metadata.artifacts.ImportValidationResult',
      },
    },
  },
});
assert.equal(abilityBridgeRequest.runtime_task.ability, 'example/validate-artifact');
assert.deepEqual(abilityBridgeRequest.runtime_task.input, { artifact: { slug: 'example-site' }, report: '/artifacts/import-report.json' });
assert.equal(abilityBridgeRequest.parent_request.executor.config.output_mappings.validation_result, 'result.import_validation_result');

const recipePackRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'recipe-task-123',
  executor: {
    backend: 'codebox',
    model: 'gpt-5.5',
    config: {
      recipe_pack: 'example-codebox-recipes',
      recipe_ref: 'release/v1',
      recipe: 'minimal-runtime',
      target_ref: 'Extra-Chill/example#42',
      recipe_secret_env: ['EXAMPLE_RECIPE_TOKEN'],
    },
  },
  inputs: {
    recipe_inputs: { fixture: 'minimal' },
  },
});
assert.deepEqual(recipePackRequest.recipe, {
  schema: 'wp-codebox/external-recipe-request/v1',
  pack: 'example-codebox-recipes',
  name: 'minimal-runtime',
  ref: 'release/v1',
  target_ref: 'Extra-Chill/example#42',
  inputs: { fixture: 'minimal' },
  secret_env: ['EXAMPLE_RECIPE_TOKEN'],
});

const codexAgentRequest = {
  ...request,
  task_id: 'codex-task-123',
  executor: {
    backend: 'codebox',
    model: 'gpt-5.5',
    config: {
      provider: 'codex',
      provider_plugin_paths: ['/components/ai-provider-for-openai'],
      secret_env: [
        'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
        'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
        'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
        'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
        'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
      ],
      agents_api: '/components/agents-api',
      agent_runtime: '/components/data-machine',
      agent_runtime_tools: '/components/data-machine-code',
      homeboy: '/components/homeboy',
      homeboy_extensions: '/components/homeboy-extensions',
      wp_codebox_bin: '/bin/wp-codebox',
      max_turns: 8,
    },
  },
};
const codexRequest = codeboxTaskRequestFromAgentTaskRequest(codexAgentRequest);
assert.equal(codexRequest.agent, 'wp-codebox-sandbox');
assert.equal(codexRequest.mode, 'sandbox');
assert.equal(codexRequest.provider, 'codex');
assert.equal(codexRequest.model, 'gpt-5.5');
assert.deepEqual(codexRequest.provider_plugin_paths, ['/components/ai-provider-for-openai']);
assert.equal(codexRequest.agents_api_path, '/components/agents-api');
assert.equal(codexRequest.runtime_component_paths.agent_runtime, '/components/data-machine');
assert.equal(codexRequest.runtime_component_paths.agent_runtime_tools, '/components/data-machine-code');
assert.equal(codexRequest.homeboy_path, '/components/homeboy');
assert.equal(codexRequest.homeboy_extensions_path, '/components/homeboy-extensions');
assert.equal(codexRequest.wp_codebox_bin, '/bin/wp-codebox');
assert.deepEqual(codexRequest.secret_env, [
  'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
  'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
  'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
]);
assert.deepEqual(codexRequest.agent_bundle, {});
assert(!JSON.stringify(codexRequest).includes('wp-ai-gateway'));

const defaultsRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-agent-task-defaults-'));
try {
  const workspaceRoot = path.join(defaultsRoot, 'target-repo@issue-1161');
  const dataMachinePath = path.join(defaultsRoot, 'data-machine');
  const bundledAgentsApiPath = path.join(dataMachinePath, 'vendor', 'wordpress', 'agents-api');
  const alternateBundledAgentsApiPath = path.join(dataMachinePath, 'vendor', 'automattic', 'agents-api');
  const dataMachineCodePath = path.join(defaultsRoot, 'data-machine-code');
  const staleStandaloneAgentsApiPath = path.join(defaultsRoot, 'agents-api');
  const providerPath = path.join(defaultsRoot, 'ai-provider-for-openai');
  for (const directory of [workspaceRoot, bundledAgentsApiPath, dataMachineCodePath, staleStandaloneAgentsApiPath, providerPath]) {
    fs.mkdirSync(directory, { recursive: true });
  }

  const defaultedRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'default-runtime-stack-task-123',
    executor: {
      backend: 'codebox',
      config: { provider: 'codex' },
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: {},
  });
  assert.equal(defaultedRequest.agents_api_path, bundledAgentsApiPath);
  assert.equal(defaultedRequest.runtime_component_paths.agents_api, bundledAgentsApiPath);
  assert.equal(defaultedRequest.runtime_component_paths.agent_runtime, dataMachinePath);
  assert.equal(defaultedRequest.runtime_component_paths.agent_runtime_tools, dataMachineCodePath);
  assert.deepEqual(defaultedRequest.provider_plugin_paths, []);
  assert.deepEqual(defaultedRequest.runtime_overlay_profiles, ['codex-subscription']);
  assert.deepEqual(defaultedRequest.runtime_overlays, []);
  assert.deepEqual(defaultedRequest.secret_env, [
    'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
    'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
    'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
  ]);
  assert.equal(defaultedRequest.mounts[0].source, workspaceRoot);
  assert.equal(defaultedRequest.mounts[0].target, '/workspace/target-repo');
  assert.equal(defaultedRequest.mounts[0].mode, 'readwrite');
  assert.equal(defaultedRequest.mounts[0].metadata.workspace_slug, 'target-repo');
  assert.deepEqual(defaultedRequest.workspaces, []);
  assert(!JSON.stringify(defaultedRequest).includes(staleStandaloneAgentsApiPath));
  assert(!JSON.stringify(defaultedRequest).includes(alternateBundledAgentsApiPath));

  const codexAuthPath = path.join(defaultsRoot, 'codex-auth.json');
  fs.writeFileSync(codexAuthPath, JSON.stringify({
    tokens: {
      access_token: 'fixture-access-token',
      refresh_token: 'fixture-refresh-token',
      account_id: 'fixture-account-id',
    },
  }));
  const codexSubscriptionDefaultedRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'codex-subscription-default-task-123',
    executor: {
      backend: 'codebox',
      config: {},
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: { wp_codebox_codex_auth_path: codexAuthPath },
  });
  assert.equal(codexSubscriptionDefaultedRequest.provider, 'codex');
  assert.equal(codexSubscriptionDefaultedRequest.model, 'gpt-5.5');
  assert.deepEqual(codexSubscriptionDefaultedRequest.provider_plugin_paths, []);
  assert.deepEqual(codexSubscriptionDefaultedRequest.runtime_overlay_profiles, ['codex-subscription']);
  assert.deepEqual(codexSubscriptionDefaultedRequest.runtime_overlays, []);
  assert.deepEqual(codexSubscriptionDefaultedRequest.secret_env, codexSecretEnv);

  const openAiDefaultedRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'default-openai-provider-task-123',
    executor: {
      backend: 'codebox',
      model: 'gpt-5.5',
      config: {},
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: { wp_codebox_codex_enabled: false },
  });
  assert.equal(openAiDefaultedRequest.provider, 'openai');
  assert.equal(openAiDefaultedRequest.model, 'gpt-5.5');
  assert.deepEqual(openAiDefaultedRequest.secret_env, ['OPENAI_API_KEY']);

  const bareOpenAiDefaultedRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'bare-default-openai-provider-task-123',
    executor: {
      backend: 'codebox',
      config: {},
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: { wp_codebox_codex_enabled: false },
  });
  assert.equal(bareOpenAiDefaultedRequest.provider, 'openai');
  assert.equal(bareOpenAiDefaultedRequest.model, '');
  assert.deepEqual(bareOpenAiDefaultedRequest.secret_env, ['OPENAI_API_KEY']);
  // A bare repo workspace defaults to a read-write mount, so coding-capable
  // workspace tools must be exposed alongside the read-only inspection tools.
  const writableWorkspaceTools = [
    'workspace_ls',
    'workspace_read',
    'workspace_git_status',
    'workspace_write',
    'workspace_edit',
    'workspace_apply_patch',
    'workspace_delete',
    'workspace_git_add',
  ];
  assert.deepEqual(bareOpenAiDefaultedRequest.allowed_tools, writableWorkspaceTools);
  assert.equal(bareOpenAiDefaultedRequest.sandbox_tool_policy.schema, 'wp-codebox/sandbox-tool-policy/v1');
  assert.deepEqual(
    bareOpenAiDefaultedRequest.sandbox_tool_policy.tools.map((tool) => tool.runtime_tool_id),
    writableWorkspaceTools,
  );
  assert.deepEqual(
    bareOpenAiDefaultedRequest.sandbox_tool_policy.tools.map((tool) => tool.runtime.environment),
    writableWorkspaceTools.map(() => 'runtime_local'),
  );

  // A read-only repo workspace must remain restricted to inspection tools.
  const readonlyWorkspaceRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'readonly-workspace-task-123',
    executor: {
      backend: 'codebox',
      config: {},
    },
    inputs: {
      target: { root: workspaceRoot, mode: 'readonly' },
    },
  }, {
    settings: { wp_codebox_codex_enabled: false },
  });
  assert.deepEqual(
    readonlyWorkspaceRequest.allowed_tools,
    ['workspace_ls', 'workspace_read', 'workspace_git_status'],
  );
  assert.deepEqual(
    readonlyWorkspaceRequest.sandbox_tool_policy.tools.map((tool) => tool.runtime_tool_id),
    ['workspace_ls', 'workspace_read', 'workspace_git_status'],
  );

  const settingsModelRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'settings-model-default-task-123',
    executor: {
      backend: 'codebox',
      config: {},
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: { wp_codebox_model: 'gpt-5.5' },
  });
  assert.equal(settingsModelRequest.model, 'gpt-5.5');

  const explicitEmptyToolsRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'explicit-empty-tools-task-123',
    executor: {
      backend: 'codebox',
      config: {
        allowed_tools: [],
        sandbox_tool_policy: {
          schema: 'wp-codebox/sandbox-tool-policy/v1',
          version: 1,
          tools: [{ id: 'deny-all', runtime_tool_id: 'deny-all', execution_location: 'parent', transport_visibility: 'hidden', allowed: false }],
          metadata: { source: 'test' },
        },
      },
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: {},
  });
  assert.deepEqual(explicitEmptyToolsRequest.allowed_tools, []);
  assert.equal(explicitEmptyToolsRequest.sandbox_tool_policy.tools[0].id, 'deny-all');

  fs.rmSync(bundledAgentsApiPath, { recursive: true, force: true });
  fs.mkdirSync(alternateBundledAgentsApiPath, { recursive: true });
  const alternateDefaultedRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'alternate-default-runtime-stack-task-123',
    executor: {
      backend: 'codebox',
      config: { provider: 'codex' },
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: {},
  });
  assert.equal(alternateDefaultedRequest.agents_api_path, alternateBundledAgentsApiPath);

  const explicitOverrideRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'explicit-runtime-stack-task-123',
    executor: {
      backend: 'codebox',
      config: {
        provider: 'codex',
        agents_api: staleStandaloneAgentsApiPath,
        runtime_component_paths: {
          agent_runtime: '/explicit/data-machine',
          agent_runtime_tools: '/explicit/data-machine-code',
        },
        provider_plugin_paths: ['/explicit/provider'],
        secret_env: ['EXPLICIT_SECRET'],
        mounts: [{ source: '/explicit/worktree', target: '/workspace', mode: 'readonly' }],
        workspaces: [{ target: '/explicit-workspace', mode: 'readonly' }],
      },
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  });
  assert.equal(explicitOverrideRequest.agents_api_path, staleStandaloneAgentsApiPath);
  assert.equal(explicitOverrideRequest.runtime_component_paths.agent_runtime, '/explicit/data-machine');
  assert.equal(explicitOverrideRequest.runtime_component_paths.agent_runtime_tools, '/explicit/data-machine-code');
  assert.deepEqual(explicitOverrideRequest.provider_plugin_paths, ['/explicit/provider']);
  assert.deepEqual(explicitOverrideRequest.secret_env, ['EXPLICIT_SECRET']);
  assert.deepEqual(explicitOverrideRequest.mounts, [{ source: '/explicit/worktree', target: '/workspace', mode: 'readonly' }]);
  assert.deepEqual(explicitOverrideRequest.workspaces, [{ target: '/explicit-workspace', mode: 'readonly' }]);
} finally {
  fs.rmSync(defaultsRoot, { recursive: true, force: true });
}
assert.equal(codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  executor: {
    ...request.executor,
    config: { ...request.executor.config, agent: 'custom-agent', mode: 'review' },
  },
}).agent, 'custom-agent');
assert.equal(codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  executor: {
    ...request.executor,
    config: { ...request.executor.config, agent: 'custom-agent', mode: 'review' },
  },
}).mode, 'review');
assert.equal(codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  limits: { task_timeout_seconds: 7 },
}).task_timeout_seconds, 7);

const agentBundleRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'agent-bundle-task-123',
  executor: {
    backend: 'codebox',
    model: 'gpt-5.5',
    config: {
      execution_kind: 'agent_bundle',
      provider: 'openai',
      provider_plugin_paths: ['/components/ai-provider-for-openai'],
      agents_api: '/components/agents-api',
      runtime_component_paths: {
        agent_runtime: '/components/data-machine',
        agent_runtime_tools: '/components/data-machine-code',
      },
      homeboy_extensions: '/components/homeboy-extensions/wordpress',
      bundle_path: '/bundles/static-site-agent',
      bundle_host_path: '/home/runner/work/wp-site-generator/wp-site-generator/bundles/static-site-agent',
      agent_slug: 'static-site-agent',
      pipeline_slug: 'static-site-pipeline',
      flow_slug: 'static-site-manual-flow',
      target_repo: 'chubes4/wp-site-generator',
      pipeline_step_patches: [{ slug: 'generate', config: { max_turns: 4 } }],
      flow_step_patches: [{ slug: 'run-pipeline', config: { step_budget: 12 } }],
      tool_recorders: [{ tool: 'github/create-pull-request', engine_data_path: 'static_site_agent.pr_url' }],
      engine_data_outputs: { static_site_pr_url: 'metadata.engine_data.static_site_agent.pr_url' },
      transcript_artifact_name: 'static-site-agent-transcript',
      replay_bundle_artifact_name: 'static-site-agent-replay',
      runner_workspace: { handle: 'wp-site-generator@site-loop', expose_to_agent: false },
    },
  },
});
assert.equal(agentBundleRequest.agent_bundle.bundle_path, '/bundles/static-site-agent');
assert.equal(agentBundleRequest.agent_bundle.agent_slug, 'static-site-agent');
assert.equal(agentBundleRequest.agent_bundle.pipeline_slug, 'static-site-pipeline');
assert.equal(agentBundleRequest.agent_bundle.flow_slug, 'static-site-manual-flow');
assert.deepEqual(agentBundleRequest.agent_bundle.pipeline_step_patches, [{ slug: 'generate', config: { max_turns: 4 } }]);
assert.deepEqual(agentBundleRequest.agent_bundle.flow_step_patches, [{ slug: 'run-pipeline', config: { step_budget: 12 } }]);
assert.deepEqual(agentBundleRequest.agent_bundle.tool_recorders, [{ tool: 'github/create-pull-request', engine_data_path: 'static_site_agent.pr_url' }]);
assert.deepEqual(agentBundleRequest.agent_bundle.engine_data_outputs, { static_site_pr_url: 'metadata.engine_data.static_site_agent.pr_url' });
assert.equal(agentBundleRequest.runtime_component_paths.agent_runtime, '/components/data-machine');
assert.equal(agentBundleRequest.runtime_component_paths.agent_runtime_tools, '/components/data-machine-code');
assert.equal(agentBundleRequest.homeboy_extensions_path, '/components/homeboy-extensions/wordpress');
assert.deepEqual(agentBundleRequest.mounts, [{
  source: '/home/runner/work/wp-site-generator/wp-site-generator/bundles/static-site-agent',
  target: '/bundles/static-site-agent',
  mode: 'readonly',
  metadata: { kind: 'agent-bundle' },
}]);

const agentBundleRequestWithExplicitMount = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  executor: {
    backend: 'codebox',
    config: {
      execution_kind: 'agent_bundle',
      mounts: [{
        source: '/custom/static-site-agent',
        target: '/bundles/static-site-agent',
        mode: 'readonly',
        metadata: { kind: 'custom' },
      }],
      bundle_path: '/bundles/static-site-agent',
      bundle_host_path: '/home/runner/work/wp-site-generator/wp-site-generator/bundles/static-site-agent',
    },
  },
});
assert.equal(agentBundleRequestWithExplicitMount.mounts.length, 1);
assert.equal(agentBundleRequestWithExplicitMount.mounts[0].source, '/custom/static-site-agent');

const outcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: false,
  provider_error: true,
  summary: 'Provider failed.',
  artifacts: { bundle: { id: 'bundle-1', directory: '/tmp/artifacts' } },
});
assert.equal(outcome.schema, 'homeboy/agent-task-outcome/v1');
assert.equal(outcome.task_id, 'task-123');
assert.equal(outcome.status, 'provider_error');
assert.equal(outcome.failure_classification, 'provider');
assert.equal(outcome.artifacts[0].id, 'bundle-1');
assert.equal(outcome.artifacts[0].path, '/tmp/artifacts');

const upstreamRunnerOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  session: {
    id: 'sandbox-session-1',
    artifacts: {
      bundle_id: 'artifact-bundle-1',
      preview_url: 'https://preview.example.test/sandbox-session-1',
    },
  },
  artifacts: '/tmp/wp-codebox-artifacts',
});
assert.equal(upstreamRunnerOutcome.status, 'succeeded');
assert.equal(upstreamRunnerOutcome.artifacts[0].kind, 'codebox-artifact-directory');
assert.equal(upstreamRunnerOutcome.artifacts[0].path, '/tmp/wp-codebox-artifacts');

const failedUpstreamRunnerOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: false,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'failed',
  session: { id: 'sandbox-session-1', status: 'failed' },
  evidence_refs: [{
    kind: 'codebox-command-evidence',
    uri: '/tmp/wp-codebox-artifacts/wp-codebox-command-evidence.json',
    label: 'codebox command evidence',
  }],
});
assert.equal(failedUpstreamRunnerOutcome.status, 'failed');
assert.equal(
  failedUpstreamRunnerOutcome.evidence_refs.some((ref) => ref.kind === 'codebox-command-evidence' && ref.uri === '/tmp/wp-codebox-artifacts/wp-codebox-command-evidence.json'),
  true
);

const emptyRunSummaryOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: false,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'failed',
}, {
  normalizeAgentTaskRunResult: () => ({
    schema: 'wp-codebox/agent-task-run-result/v1',
    status: 'failed',
    failure_classification: 'runtime',
    artifacts: [],
    diagnostics: [],
    metadata: {
      provider_error: {},
      run_id: '',
      run_status: '',
      runtime_id: '',
      runtime_status: '',
    },
    refs: {
      artifact_bundles: [],
      changed_files: [],
      logs: [],
      patches: [],
      runtimes: [],
      transcripts: [],
    },
  }),
  exitStatus: 1,
});
assert.equal(emptyRunSummaryOutcome.status, 'failed');
assert.equal(emptyRunSummaryOutcome.metadata.codebox_run_result.diagnostics[0].class, 'codebox.no_runtime_session');
assert.equal(emptyRunSummaryOutcome.metadata.codebox_run_result.metadata.provider_error.code, 'codebox_no_runtime_session');
assert.equal(emptyRunSummaryOutcome.diagnostics[0].class, 'codebox.no_runtime_session');

const runtimeRefRunSummaryOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: false,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'failed',
}, {
  normalizeAgentTaskRunResult: () => ({
    schema: 'wp-codebox/agent-task-run-result/v1',
    status: 'failed',
    failure_classification: 'runtime',
    artifacts: [],
    diagnostics: [],
    metadata: {
      run_id: 'codebox-run-1',
      runtime_id: 'runtime-1',
    },
    refs: {
      logs: ['homeboy://codebox/runs/codebox-run-1/logs'],
      transcripts: [],
      artifact_bundles: [],
    },
  }),
  exitStatus: 1,
});
assert.equal(runtimeRefRunSummaryOutcome.metadata.codebox_run_result.metadata.runtime_id, 'runtime-1');
assert.equal(runtimeRefRunSummaryOutcome.metadata.codebox_run_result.refs.logs[0], 'homeboy://codebox/runs/codebox-run-1/logs');
assert.equal(runtimeRefRunSummaryOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.no_runtime_session'), false);

const recipeProbeFailureOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  recipe_run: {
    pack: 'example-codebox-recipes',
    name: 'minimal-runtime',
    ref: 'release/v1',
    target_ref: 'Extra-Chill/example#42',
    startup: { success: true, log_path: '/tmp/recipe/startup.log' },
    probes: [{ id: 'home-page', status: 'failed', path: '/tmp/recipe/probes/home-page.json', screenshot: '/tmp/recipe/screens/home-page.png' }],
    fake_side_effects: '/tmp/recipe/fakes/side-effects.json',
    declared_artifacts: [{ name: 'runtime-log', path: '/tmp/recipe/logs/runtime.log' }],
  },
});
assert.equal(recipeProbeFailureOutcome.status, 'failed');
assert.equal(recipeProbeFailureOutcome.summary, 'WP Codebox home-page failed.');
assert.equal(recipeProbeFailureOutcome.failure_classification, 'execution_failed');
assert.equal(recipeProbeFailureOutcome.metadata.recipe_failed_phase, 'probe');
assert.equal(recipeProbeFailureOutcome.metadata.decision_evidence.recipe_pack, 'example-codebox-recipes');
assert.equal(recipeProbeFailureOutcome.metadata.decision_evidence.recipe_failed_phase, 'probe');
assert.equal(recipeProbeFailureOutcome.diagnostics[0].class, 'codebox.recipe.probe.failed');
assert.equal(recipeProbeFailureOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-recipe-startup-log' && artifact.path === '/tmp/recipe/startup.log'), true);
assert.equal(recipeProbeFailureOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-recipe-probe-json' && artifact.path === '/tmp/recipe/probes/home-page.json'), true);
assert.equal(recipeProbeFailureOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-recipe-screenshot' && artifact.path === '/tmp/recipe/screens/home-page.png'), true);
assert.equal(recipeProbeFailureOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-recipe-fake-side-effects' && artifact.path === '/tmp/recipe/fakes/side-effects.json'), true);
assert.equal(recipeProbeFailureOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-recipe-artifact' && artifact.path === '/tmp/recipe/logs/runtime.log'), true);
assert.equal(recipeProbeFailureOutcome.evidence_refs.some((ref) => ref.uri === '/tmp/recipe/screens/home-page.png'), true);

const recipeStartupFailureOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: false,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  metadata: {
    recipe_run: {
      name: 'minimal-runtime',
      startup: { success: false, message: 'Recipe could not boot WordPress.', log: '/tmp/recipe/startup.log' },
    },
  },
});
assert.equal(recipeStartupFailureOutcome.status, 'failed');
assert.equal(recipeStartupFailureOutcome.summary, 'Recipe could not boot WordPress.');
assert.equal(recipeStartupFailureOutcome.metadata.recipe_failed_phase, 'startup');

const recipeArtifactCollectionFailureOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: false,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  recipe_run: {
    name: 'minimal-runtime',
    artifact_collection: { success: false, summary: 'Declared artifact path was missing.' },
  },
});
assert.equal(recipeArtifactCollectionFailureOutcome.status, 'failed');
assert.equal(recipeArtifactCollectionFailureOutcome.summary, 'Declared artifact path was missing.');
assert.equal(recipeArtifactCollectionFailureOutcome.metadata.recipe_failed_phase, 'artifact_collection');

const normalizedCompletedOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  summary: 'WP Codebox agent task succeeded.',
  outputs: {
    issue_number: 123,
    issue_url: 'https://github.com/chubes4/wp-site-generator/issues/123',
  },
  session: { id: 'sandbox-session-1', status: 'completed' },
}, { exitStatus: 1 });
assert.equal(normalizedCompletedOutcome.status, 'succeeded');
assert.equal(normalizedCompletedOutcome.outputs.issue_number, 123);
assert.equal(normalizedCompletedOutcome.failure_classification, undefined);

const completedFailureOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: false,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  summary: 'Agent bundle workload did not return any scenarios.',
});
assert.equal(completedFailureOutcome.status, 'failed');
assert.equal(completedFailureOutcome.failure_classification, 'execution_failed');

const nestedAgentRuntimeFailureOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  summary: 'Outer runner completed.',
  run: {
    agentResult: {
      success: false,
      status: 'completed_no_items',
      completion_outcome: {
        status: 'completed_no_items',
        success: false,
      },
    },
  },
});
assert.equal(nestedAgentRuntimeFailureOutcome.status, 'failed');
assert.equal(nestedAgentRuntimeFailureOutcome.failure_classification, 'execution_failed');

const nestedAgentRuntimeOutputFailureOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  summary: 'Outer runner completed.',
  outputs: {
    success: false,
    status: 'completed_no_items',
    completion_outcome: {
      status: 'completed_no_items',
      success: false,
    },
  },
});
assert.equal(nestedAgentRuntimeOutputFailureOutcome.status, 'failed');
assert.equal(nestedAgentRuntimeOutputFailureOutcome.failure_classification, 'execution_failed');

const rawNestedAgentRuntimeFailureOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  summary: 'WP Codebox agent task succeeded.',
  raw: {
    agent_runtime: {
      success: true,
      result: {
        success: false,
        status: 'completed_no_items',
        completion_outcome: {
          status: 'completed_no_items',
          success: false,
        },
      },
    },
  },
});
assert.equal(rawNestedAgentRuntimeFailureOutcome.status, 'failed');
assert.equal(rawNestedAgentRuntimeFailureOutcome.failure_classification, 'execution_failed');

const agentBundleOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'agent-bundle-task-123',
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  artifacts: '/tmp/wp-codebox-artifacts',
  task_input: {
    policy: { write: 'sandbox', apply: 'review' },
    sandbox_tool_policy: {
      schema: 'wp-codebox/sandbox-tool-policy/v1',
      tools: [{ id: 'homeboy/no-runtime-tools', allowed: false }],
    },
  },
  metadata: {
    agent_runtime: {
      bundle: agentBundleRequest.agent_bundle,
      workload: {
        outputs: {
          typed_artifacts: {
            static_site_candidate: {
              schema: 'homeboy/agent-task-typed-artifact/v1',
              type: 'StaticSiteCandidate',
              artifact_schema: 'static-site-importer/static-site-candidate/v1',
              payload: { slug: 'issue-1222-transformer-loop', import_ready: true },
              provenance: { bundle_slug: 'static-site-agent', task_id: 'agent-bundle-task-123' },
              file_refs: [{ path: '/tmp/wp-codebox-artifacts/static-site-candidate.json', mime: 'application/json' }],
            },
          },
        },
        scenarios: [{
          id: 'agent-bundle',
          metadata: {
            transcript_artifacts: { json: '/tmp/transcript.json', summary: '/tmp/transcript.md' },
            replay_bundle_path: '/tmp/replay-bundle',
            engine_data: { static_site_agent: { pr_url: 'https://github.com/chubes4/wp-site-generator/pull/123' } },
          },
        }],
      },
    },
  },
});
assert.equal(agentBundleOutcome.schema, 'homeboy/agent-task-outcome/v1');
assert.equal(agentBundleOutcome.status, 'succeeded');
assert.equal(agentBundleOutcome.outputs.static_site_pr_url, 'https://github.com/chubes4/wp-site-generator/pull/123');
assert.equal(agentBundleOutcome.outputs.typed_artifacts.static_site_candidate.type, 'StaticSiteCandidate');
assert.equal(agentBundleOutcome.outputs.typed_artifacts.static_site_candidate.artifact_schema, 'static-site-importer/static-site-candidate/v1');
assert.equal(agentBundleOutcome.outputs.typed_artifacts.static_site_candidate.payload.import_ready, true);
assert.equal(agentBundleOutcome.artifacts.some((artifact) => artifact.kind === 'typed-bundle-output' && artifact.name === 'static_site_candidate' && artifact.path === '/tmp/wp-codebox-artifacts/static-site-candidate.json'), true);
assert.equal(agentBundleOutcome.artifacts.some((artifact) => artifact.kind === 'agent-runtime-transcript' && artifact.path === '/tmp/transcript.json'), true);
assert.equal(agentBundleOutcome.artifacts.some((artifact) => artifact.kind === 'agent-runtime-replay-bundle' && artifact.path === '/tmp/replay-bundle'), true);
assert.equal(agentBundleOutcome.evidence_refs.some((ref) => ref.uri === 'https://github.com/chubes4/wp-site-generator/pull/123'), true);
assert.equal(agentBundleOutcome.evidence_refs.some((ref) => ref.uri === '/tmp/wp-codebox-artifacts/static-site-candidate.json'), true);
assert.equal(agentBundleOutcome.metadata.sandbox_policy.policy.apply, 'review');
assert.equal(agentBundleOutcome.metadata.sandbox_policy.sandbox_tool_policy.tools[0].allowed, false);
assert.equal(upstreamRunnerOutcome.artifacts[1].kind, 'codebox-session-artifacts');
assert.equal(upstreamRunnerOutcome.evidence_refs[0].uri, 'https://preview.example.test/sandbox-session-1');
assert.equal(upstreamRunnerOutcome.evidence_refs[1].uri, '/tmp/wp-codebox-artifacts');

const canonicalTopLevelAgentBundleOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'canonical-top-level-agent-bundle-task-123',
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  artifacts: '/tmp/wp-codebox-artifacts',
  metadata: {
    agent_runtime: {
      bundle: {
        engine_data_outputs: {
          static_site_branch: 'metadata.engine_data.static_site_agent.branch',
          static_site_pr_url: 'metadata.engine_data.static_site_agent.pr_url',
          static_site_slug: 'metadata.engine_data.static_site_agent.slug',
        },
      },
      workload: {
        outputs: [],
      },
    },
  },
  outputs: {
    engine_data: {
      static_site_agent: {
        branch: 'static/issue-451-design-direction',
        pr_url: 'https://github.com/chubes4/wp-site-generator/pull/453',
        slug: 'issue-451-design-direction',
      },
    },
  },
});
assert.equal(canonicalTopLevelAgentBundleOutcome.status, 'succeeded');
assert.equal(canonicalTopLevelAgentBundleOutcome.outputs.static_site_branch, 'static/issue-451-design-direction');
assert.equal(canonicalTopLevelAgentBundleOutcome.outputs.static_site_pr_url, 'https://github.com/chubes4/wp-site-generator/pull/453');
assert.equal(canonicalTopLevelAgentBundleOutcome.outputs.static_site_slug, 'issue-451-design-direction');
assert.equal(canonicalTopLevelAgentBundleOutcome.evidence_refs.some((ref) => ref.uri === 'https://github.com/chubes4/wp-site-generator/pull/453'), true);

const singleResultAgentBundleOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'single-result-agent-bundle-task-123',
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  artifacts: '/tmp/wp-codebox-artifacts',
  metadata: {
    agent_runtime: {
      bundle: {
        engine_data_outputs: {
          issue_number: 'metadata.engine_data.store_idea_agent.issue_number',
          issue_url: 'metadata.engine_data.store_idea_agent.issue_url',
        },
      },
      workload: {
        outputs: {
          issue_number: 123,
          issue_url: 'https://github.com/chubes4/wp-site-generator/issues/123',
        },
        diagnostics: [{ class: 'agent_runtime.output', message: 'Semantic outputs captured.' }],
      },
    },
  },
});
assert.equal(singleResultAgentBundleOutcome.schema, 'homeboy/agent-task-outcome/v1');
assert.equal(singleResultAgentBundleOutcome.status, 'succeeded');
assert.equal(singleResultAgentBundleOutcome.outputs.issue_number, 123);
assert.equal(singleResultAgentBundleOutcome.outputs.issue_url, 'https://github.com/chubes4/wp-site-generator/issues/123');
assert.equal(singleResultAgentBundleOutcome.evidence_refs.some((ref) => ref.uri === 'https://github.com/chubes4/wp-site-generator/issues/123'), true);

const canaryRunOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  session: {
    id: 'homeboy-codebox-canary-1042',
    artifacts: {
      bundle_id: 'artifact-bundle-sha256-canary',
      preview_url: '',
    },
  },
  artifacts: {
    id: 'artifact-bundle-sha256-canary',
    runtimeLogPath: '/tmp/canary/runtime/logs/runtime.log',
    commandsLogPath: '/tmp/canary/runtime/logs/commands.log',
  },
  run: {
    runId: 'run-canary',
    status: 'succeeded',
    heartbeatAt: '2026-06-03T11:19:00.998Z',
    runtime: { id: 'runtime-canary', status: 'destroyed' },
    artifactRefs: [{
      kind: 'artifact-bundle',
      directory: '/tmp/canary/runtime',
      id: 'artifact-bundle-sha256-canary',
      digest: { algorithm: 'sha256', value: 'canary-digest' },
    }],
    agentResult: {
      summary: 'Agent sandbox completed without actionable file changes.',
      changedFiles: { count: 0, paths: [], artifact: 'files/changed-files.json' },
      patch: { bytes: 0, sha256: 'empty-patch-sha', artifact: 'files/patch.diff' },
      transcript: { artifact: 'files/transcript.json', executionCount: 1 },
      artifacts: { directory: '/tmp/canary/runtime' },
      noOpReason: 'no_file_changes',
    },
  },
  completionOutcome: {
    status: 'partial',
    nextAction: 'review',
    confidence: 'medium',
    provenance: {
      artifactBundleId: 'artifact-bundle-sha256-canary',
      artifactDirectory: '/tmp/canary/runtime',
    },
  },
});
assert.equal(canaryRunOutcome.status, 'no_op');
assert.equal(canaryRunOutcome.artifacts.some((artifact) => artifact.kind === 'artifact-bundle' && artifact.path === '/tmp/canary/runtime'), true);
assert.equal(canaryRunOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-changed-files' && artifact.path === '/tmp/canary/runtime/files/changed-files.json'), true);
assert.equal(canaryRunOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-patch' && artifact.path === '/tmp/canary/runtime/files/patch.diff'), true);
assert.equal(canaryRunOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-transcript' && artifact.path === '/tmp/canary/runtime/files/transcript.json'), true);
assert.equal(canaryRunOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-runtime-log'), true);
assert.equal(canaryRunOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-command-log'), true);
assert.equal(canaryRunOutcome.evidence_refs.some((ref) => ref.uri === '/tmp/canary/runtime/files/patch.diff'), true);
assert.equal(canaryRunOutcome.metadata.decision_evidence.selected_backend, 'codebox');
assert.equal(canaryRunOutcome.metadata.decision_evidence.run_id, 'run-canary');
assert.equal(canaryRunOutcome.metadata.decision_evidence.runtime_status, 'destroyed');
assert.equal(canaryRunOutcome.metadata.decision_evidence.cleanup_observed, 'runtime_destroyed');
assert.equal(canaryRunOutcome.metadata.decision_evidence.no_op_reason, 'no_file_changes');
assert.equal(canaryRunOutcome.metadata.decision_evidence.patch_bytes, 0);
assert.deepEqual(canaryRunOutcome.metadata.decision_evidence.runtime_gap_trackers, []);

const codexOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  summary: 'Codex task completed.',
  artifacts: [{
    id: 'codex-artifact-1',
    metadata: {
      secretEnvValues: {
        AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'artifact-access-token-value',
      },
    },
  }],
  metadata: {
    provider: 'codex',
    secret_env: ['AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN'],
    secret_env_values: {
      AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'codex-access-token-value',
      AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'codex-refresh-token-value',
    },
  },
  diagnostics: [{
    class: 'codex',
    message: 'Codex token diagnostics.',
    data: {
      secretEnvValues: {
        AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'diagnostic-access-token-value',
      },
    },
  }],
});
const serializedCodexOutcome = JSON.stringify(codexOutcome);
assert(serializedCodexOutcome.includes('AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN'));
assert(!serializedCodexOutcome.includes('codex-access-token-value'));
assert(!serializedCodexOutcome.includes('codex-refresh-token-value'));
assert(!serializedCodexOutcome.includes('artifact-access-token-value'));
assert(!serializedCodexOutcome.includes('diagnostic-access-token-value'));

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-agent-task-executor-'));
try {
  const { fixture, capture } = writeFixtureTaskRunner(root);
  const missingModelResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify({
      ...request,
      task_id: 'missing-model-cli-task-123',
      executor: {
        backend: 'codebox',
        config: {
          provider: 'claude-code',
          provider_plugin_paths: ['/providers/claude-code'],
        },
      },
    }),
  });
  assert.equal(missingModelResult.status, 1, missingModelResult.stderr || missingModelResult.stdout);
  const missingModelOutcome = JSON.parse(missingModelResult.stdout);
  assert.equal(missingModelOutcome.status, 'failed');
  assert.equal(missingModelOutcome.failure_classification, 'provider');
  assert.equal(missingModelOutcome.diagnostics[0].class, 'codebox.preflight.missing_model');
  assert.match(missingModelOutcome.summary, /--model/);
  assert.match(missingModelOutcome.summary, /provider-config\.model/);
  assert.equal(fs.existsSync(capture), false);

  const result = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    fixture,
    '--agents-api',
    '/components/agents-api',
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify(request),
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const cliOutcome = JSON.parse(result.stdout);
  assert.equal(cliOutcome.status, 'succeeded');
  assert.equal(cliOutcome.artifacts[0].kind, 'screenshot');
  assert.equal(cliOutcome.evidence_refs[0].uri, 'https://example.test/preview');

  const normalizedResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: fixtureEnv({ HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCodeboxCoreModule }),
  });
  assert.equal(normalizedResult.status, 0, normalizedResult.stderr || normalizedResult.stdout);
  const normalizedOutcome = JSON.parse(normalizedResult.stdout);
  assert.equal(normalizedOutcome.metadata.codebox_run_result.schema, 'wp-codebox/agent-task-run-result/v1');
  assert.equal(normalizedOutcome.metadata.codebox_recipe_run_summary.schema, 'wp-codebox/recipe-run-summary/v1');
  assert.equal(normalizedOutcome.metadata.decision_evidence.run_id, 'fixture-run-1');
  assert.equal(normalizedOutcome.metadata.decision_evidence.runtime_status, 'destroyed');
  assert.equal(normalizedOutcome.metadata.decision_evidence.patch_sha256, 'fixture-patch-sha');
  assert.equal(normalizedOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-patch' && artifact.path === '/tmp/fixture-normalized/patch.diff'), true);
  assert.equal(normalizedOutcome.artifacts.some((artifact) => artifact.kind === 'recipe-probe-result' && artifact.path === '/tmp/fixture-normalized/recipe-probe.json'), true);
  assert.equal(normalizedOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'fixture.normalizer'), true);
  assert.equal(normalizedOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'fixture.recipe_normalizer'), true);

  const captured = JSON.parse(fs.readFileSync(capture, 'utf8'));
  assert.equal(captured.request.schema, 'wp-codebox/task-input/v1');
  assert.equal(captured.request.orchestrator.agent_task_id, 'task-123');
  assert.equal(captured.request.runtime_overlays[0].type, 'bundled-library');

  const recipeCliResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify({
      ...request,
      task_id: 'recipe-cli-task-123',
      executor: {
        backend: 'codebox',
        config: {
          recipe_pack: 'example-codebox-recipes',
          recipe_ref: 'release/v1',
          recipe: 'minimal-runtime',
          target_ref: 'Extra-Chill/example#42',
          recipe_secret_env: [],
        },
      },
    }),
  });
  assert.equal(recipeCliResult.status, 0, recipeCliResult.stderr || recipeCliResult.stdout);
  const capturedRecipe = JSON.parse(fs.readFileSync(capture, 'utf8'));
  assert.equal(capturedRecipe.request.recipe.pack, 'example-codebox-recipes');
  assert.equal(capturedRecipe.request.recipe.name, 'minimal-runtime');
  assert.equal(capturedRecipe.request.recipe.target_ref, 'Extra-Chill/example#42');

  const fakeCodexHome = path.join(root, 'fake-codex-home');
  const fakeCodexDirectory = path.join(fakeCodexHome, '.codex');
  fs.mkdirSync(fakeCodexDirectory, { recursive: true });
  fs.writeFileSync(path.join(fakeCodexDirectory, 'auth.json'), JSON.stringify({
    tokens: {
      access_token: 'fixture-codex-access-token',
      refresh_token: 'fixture-codex-refresh-token',
      expires_at: '1780000000',
      account_id: 'fixture-codex-account-id',
    },
  }));

  const codexResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
    env: fixtureEnv({ HOME: fakeCodexHome }),
    input: JSON.stringify({
      ...codexAgentRequest,
      task_id: 'codex-cli-task-123',
    }),
  });
  assert.equal(codexResult.status, 0, codexResult.stderr || codexResult.stdout);
  const capturedCodex = JSON.parse(fs.readFileSync(capture, 'utf8'));
  assert.equal(capturedCodex.request.provider, 'codex');
  assert.equal(capturedCodex.request.model, 'gpt-5.5');
  assert.deepEqual(capturedCodex.request.provider_plugin_paths, ['/components/ai-provider-for-openai']);
  assert.equal(capturedCodex.request.agents_api_path, '/components/agents-api');
  assert.equal(capturedCodex.request.data_machine_path, '/components/data-machine');
  assert.equal(capturedCodex.request.data_machine_code_path, '/components/data-machine-code');
  assert.equal(capturedCodex.request.runtime_component_paths.agent_runtime, '/components/data-machine');
  assert.equal(capturedCodex.request.runtime_component_paths.agent_runtime_tools, '/components/data-machine-code');
  assert.equal(capturedCodex.request.homeboy_path, '/components/homeboy');
  assert.equal(capturedCodex.request.homeboy_extensions_path, '/components/homeboy-extensions');
  assert.deepEqual(capturedCodex.request.secret_env, [
    'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
    'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
    'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
  ]);
  assert.deepEqual(capturedCodex.env_presence, Object.fromEntries(codexSecretEnv.map((name) => [name, true])));
  assert(!JSON.stringify(capturedCodex).includes('wp-ai-gateway'));
  assert(!JSON.stringify(capturedCodex).includes('fixture-codex-access-token'));

  const agentBundleRoot = fs.mkdtempSync(path.join(root, 'agent-bundle-'));
  const bundle = writeBundleFixture(agentBundleRoot);
  const { fixture: fakeWpCodebox, capture: fakeWpCodeboxCapture } = writeFakeWpCodebox(agentBundleRoot);
  const agentBundleCliRequest = {
    ...request,
    task_id: 'agent-bundle-cli-task-123',
    executor: {
      backend: 'codebox',
      model: 'gpt-5.5',
      config: {
        provider: 'openai',
        provider_plugin_paths: ['/components/ai-provider-for-openai'],
        agents_api: '/components/agents-api',
        runtime_component_paths: {
          agent_runtime: '/components/data-machine',
          agent_runtime_tools: '/components/data-machine-code',
        },
        homeboy_extensions: path.join(__dirname, '..'),
        wp_codebox_bin: fakeWpCodebox,
        bundle_path: bundle,
        agent_slug: 'static-site-agent',
        pipeline_slug: 'static-site-pipeline',
        flow_slug: 'static-site-manual-flow',
        tool_recorders: [{ tool: 'github/create-pull-request', engine_data_path: 'static_site_agent.pr_url' }],
        engine_data_outputs: { static_site_pr_url: 'metadata.engine_data.static_site_agent.pr_url' },
      },
    },
  };
  const agentBundleCliResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify(agentBundleCliRequest),
  });
  assert.equal(agentBundleCliResult.status, 0, agentBundleCliResult.stderr || agentBundleCliResult.stdout);
  const agentBundleCliOutcome = JSON.parse(agentBundleCliResult.stdout);
  assert.equal(agentBundleCliOutcome.status, 'succeeded');
  assert.equal(agentBundleCliOutcome.artifacts.some((artifact) => artifact.kind === 'agent-runtime-transcript'), true);
  assert.equal(agentBundleCliOutcome.evidence_refs.some((ref) => ref.uri === 'https://github.com/chubes4/wp-site-generator/pull/123'), true);
  const capturedAgentBundleRun = JSON.parse(fs.readFileSync(fakeWpCodeboxCapture, 'utf8'));
  assert.equal(capturedAgentBundleRun.argv[0], 'agent-task-run');
  assert.equal(capturedAgentBundleRun.input.schema, 'wp-codebox/task-input/v1');
  assert.equal(capturedAgentBundleRun.input.agent_bundle.bundle_path, bundle);
  assert.equal(capturedAgentBundleRun.input.agent_bundle.agent_slug, 'static-site-agent');
  assert.equal(capturedAgentBundleRun.input.agent_bundle.pipeline_slug, 'static-site-pipeline');
  assert.deepEqual(capturedAgentBundleRun.input.agent_bundle.tool_recorders, [{ tool: 'github/create-pull-request', engine_data_path: 'static_site_agent.pr_url' }]);

  const recipeWpCodeboxRoot = fs.mkdtempSync(path.join(root, 'recipe-wp-codebox-'));
  const { fixture: recipeFakeWpCodebox, capture: recipeFakeWpCodeboxCapture } = writeFakeWpCodebox(recipeWpCodeboxRoot);
  const recipeWpCodeboxResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify({
      ...request,
      task_id: 'recipe-wp-codebox-task-123',
      executor: {
        backend: 'codebox',
        config: {
          wp_codebox_bin: recipeFakeWpCodebox,
          recipe_pack: 'example-codebox-recipes',
          recipe_ref: 'release/v1',
          recipe: 'minimal-runtime',
          target_ref: 'Extra-Chill/example#42',
          recipe_secret_env: [],
        },
      },
    }),
  });
  assert.equal(recipeWpCodeboxResult.status, 0, recipeWpCodeboxResult.stderr || recipeWpCodeboxResult.stdout);
  const capturedRecipeWpCodeboxRun = JSON.parse(fs.readFileSync(recipeFakeWpCodeboxCapture, 'utf8'));
  assert.equal(capturedRecipeWpCodeboxRun.argv[0], 'agent-task-run');
  assert.equal(capturedRecipeWpCodeboxRun.input.recipe.pack, 'example-codebox-recipes');
  assert.equal(capturedRecipeWpCodeboxRun.input.recipe.name, 'minimal-runtime');
  assert.equal(capturedRecipeWpCodeboxRun.input.recipe.target_ref, 'Extra-Chill/example#42');

  const failedWpCodeboxRoot = fs.mkdtempSync(path.join(root, 'failed-wp-codebox-'));
  const { fixture: failedFakeWpCodebox } = writeFakeWpCodebox(failedWpCodeboxRoot);
  const failedWpCodeboxResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
  ], {
    encoding: 'utf8',
    env: fixtureEnv({ FIXTURE_WP_CODEBOX_AGENT_TASK_FAILURE: '1' }),
    input: JSON.stringify({
      ...request,
      task_id: 'failed-wp-codebox-task-123',
      executor: {
        backend: 'codebox',
        config: {
          wp_codebox_bin: failedFakeWpCodebox,
          homeboy_extensions: path.join(__dirname, '..'),
        },
      },
    }),
  });
  assert.equal(failedWpCodeboxResult.status, 1, failedWpCodeboxResult.stderr || failedWpCodeboxResult.stdout);
  const failedWpCodeboxOutcome = JSON.parse(failedWpCodeboxResult.stdout);
  assert.equal(failedWpCodeboxOutcome.status, 'failed');
  assert.equal(failedWpCodeboxOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-command-evidence'), true);
  assert.equal(failedWpCodeboxOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-agent-task-input'), true);
  assert.equal(failedWpCodeboxOutcome.evidence_refs.some((ref) => ref.kind === 'codebox-command-evidence'), true);
  assert.equal(failedWpCodeboxOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'wp-codebox.command.evidence_preserved'), true);

  const emptyJsonWpCodeboxResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify({
      ...request,
      task_id: 'empty-json-wp-codebox-task-123',
      executor: {
        backend: 'codebox',
        config: {
          wp_codebox_bin: writeEmptyJsonTaskRunner(root),
          homeboy_extensions: path.join(__dirname, '..'),
        },
      },
    }),
  });
  assert.equal(emptyJsonWpCodeboxResult.status, 1, emptyJsonWpCodeboxResult.stderr || emptyJsonWpCodeboxResult.stdout);
  const emptyJsonWpCodeboxOutcome = JSON.parse(emptyJsonWpCodeboxResult.stdout);
  assert.equal(emptyJsonWpCodeboxOutcome.status, 'failed');
  assert.equal(emptyJsonWpCodeboxOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'wp-codebox.agent_task_run_empty_json'), true);
  assert.equal(emptyJsonWpCodeboxOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-command-evidence'), true);
  assert.equal(emptyJsonWpCodeboxOutcome.evidence_refs.some((ref) => ref.kind === 'codebox-command-evidence'), true);

  const emptyStdoutWpCodeboxResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify({
      ...request,
      task_id: 'empty-stdout-wp-codebox-task-123',
      executor: {
        backend: 'codebox',
        config: {
          wp_codebox_bin: writeEmptyStdoutTaskRunner(root),
          homeboy_extensions: path.join(__dirname, '..'),
        },
      },
    }),
  });
  assert.equal(emptyStdoutWpCodeboxResult.status, 1, emptyStdoutWpCodeboxResult.stderr || emptyStdoutWpCodeboxResult.stdout);
  const emptyStdoutWpCodeboxOutcome = JSON.parse(emptyStdoutWpCodeboxResult.stdout);
  assert.equal(emptyStdoutWpCodeboxOutcome.status, 'failed');
  assert.equal(emptyStdoutWpCodeboxOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'wp-codebox.agent_task_run_empty_stdout'), true);
  assert.equal(emptyStdoutWpCodeboxOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-command-evidence'), true);
  assert.equal(emptyStdoutWpCodeboxOutcome.evidence_refs.some((ref) => ref.kind === 'codebox-command-evidence'), true);

  const contractResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--print-contract',
  ], { encoding: 'utf8' });
  assert.equal(contractResult.status, 0, contractResult.stderr || contractResult.stdout);
  const printedContract = JSON.parse(contractResult.stdout);
  assert.equal(printedContract.id, 'wordpress.codebox-agent-task-executor');
  assert.deepEqual(printedContract.outcome_statuses, provider.outcome_statuses);
  assert.deepEqual(printedContract.failure_classifications, provider.failure_classifications);

  const artifactRoot = path.join(root, 'timeout-artifacts');
  const bundleRoot = writeTimeoutArtifacts(artifactRoot, 'task-timeout');
  const hangingRequest = {
    ...request,
    task_id: 'task-timeout',
    limits: { task_timeout_seconds: 1 },
  };
  const timeoutResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    writeHangingTaskRunner(root),
    '--artifacts',
    artifactRoot,
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify(hangingRequest),
    timeout: 3000,
  });
  assert.equal(timeoutResult.status, 1, timeoutResult.stderr || timeoutResult.stdout);
  const timeoutOutcome = JSON.parse(timeoutResult.stdout);
  assert.equal(timeoutOutcome.status, 'timeout');
  assert.equal(timeoutOutcome.failure_classification, 'timeout');
  assert.equal(timeoutOutcome.diagnostics[0].class, 'codebox.timeout');
  assert.equal(timeoutOutcome.diagnostics[0].data.timeout_ms, 1000);
  assert.equal(timeoutOutcome.artifacts[0].path, artifactRoot);
  assert.equal(timeoutOutcome.metadata.codebox.evidence_path, path.join(artifactRoot, 'homeboy-codebox-task-runner.json'));
  assert.equal(timeoutOutcome.metadata.codebox.timeout_classification, 'provider_timeout');
  assert.equal(timeoutOutcome.metadata.codebox.runtime_id, 'runtime-task-timeout');
  assert.equal(timeoutOutcome.metadata.codebox.last_known_phase, 'agent.inspecting-runtime');
  assert.equal(timeoutOutcome.metadata.codebox.last_heartbeat.turn, 3);
  assert.equal(timeoutOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-artifact-bundle' && artifact.path === bundleRoot), true);
  assert.equal(timeoutOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-runtime-reference-manifest'), true);
  assert.equal(timeoutOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-command-log'), true);
  assert.equal(timeoutOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-transcript'), true);
  assert.equal(timeoutOutcome.evidence_refs.some((ref) => ref.kind === 'codebox-transcript'), true);

  const configArtifactRoot = path.join(root, 'timeout-config-artifacts');
  const configBundleRoot = writeTimeoutArtifacts(configArtifactRoot, 'task-timeout-config-artifacts');
  const configArtifactRequest = {
    ...request,
    task_id: 'task-timeout-config-artifacts',
    limits: { task_timeout_seconds: 1 },
    executor: {
      ...request.executor,
      config: {
        ...request.executor.config,
        artifacts: configArtifactRoot,
      },
    },
  };
  const configArtifactTimeoutResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    writeHangingTaskRunner(root),
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify(configArtifactRequest),
    timeout: 3000,
  });
  assert.equal(configArtifactTimeoutResult.status, 1, configArtifactTimeoutResult.stderr || configArtifactTimeoutResult.stdout);
  const configArtifactTimeoutOutcome = JSON.parse(configArtifactTimeoutResult.stdout);
  assert.equal(configArtifactTimeoutOutcome.status, 'timeout');
  assert.equal(configArtifactTimeoutOutcome.artifacts[0].path, configArtifactRoot);
  assert.equal(configArtifactTimeoutOutcome.metadata.codebox.artifacts, configArtifactRoot);
  assert.equal(configArtifactTimeoutOutcome.metadata.codebox.evidence_path, path.join(configArtifactRoot, 'homeboy-codebox-task-runner.json'));
  assert.equal(configArtifactTimeoutOutcome.metadata.codebox.artifact_ref_count > 0, true);
  assert.equal(configArtifactTimeoutOutcome.metadata.codebox.runtime_id, 'runtime-task-timeout-config-artifacts');
  assert.equal(configArtifactTimeoutOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-artifact-bundle' && artifact.path === configBundleRoot), true);
  assert.equal(configArtifactTimeoutOutcome.evidence_refs.some((ref) => ref.kind === 'codebox-transcript'), true);

  const missingSecretResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    writeMissingSecretTaskRunner(root),
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify(codexAgentRequest),
  });
  assert.equal(missingSecretResult.status, 1, missingSecretResult.stderr || missingSecretResult.stdout);
  const missingSecretOutcome = JSON.parse(missingSecretResult.stdout);
  assert.equal(missingSecretOutcome.status, 'failed');
  assert.equal(missingSecretOutcome.failure_classification, 'provider');
  assert.equal(missingSecretOutcome.diagnostics[0].class, 'codebox.preflight.missing_secret_env');
  assert.deepEqual(missingSecretOutcome.diagnostics[0].data.missing_env, [
    'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  ]);
  assert.equal(missingSecretOutcome.diagnostics[0].data.phase, 'codebox.preflight');
  assert.equal(missingSecretOutcome.metadata.codebox.missing_env[0], 'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN');
  assert(!JSON.stringify(missingSecretOutcome).includes('codex-access-token-value'));

  const missingWorkspace = writeFixtureTaskRunner(root);
  const missingWorkspaceRequest = {
    ...request,
    task_id: 'missing-workspace-task-123',
    group_key: 'a8c-intelligence',
    workspace: {
      mode: 'existing',
      slug: 'a8c-intelligence',
      materialization: { repo: 'a8c-intelligence' },
    },
    executor: {
      ...request.executor,
      config: {
        provider: 'openai',
        task_kind: 'repo-cooking',
        repo: 'a8c-intelligence',
      },
    },
  };
  fs.rmSync(missingWorkspace.capture, { force: true });
  const missingWorkspaceResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    missingWorkspace.fixture,
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify(missingWorkspaceRequest),
  });
  assert.equal(missingWorkspaceResult.status, 1, missingWorkspaceResult.stderr || missingWorkspaceResult.stdout);
  assert.equal(fs.existsSync(missingWorkspace.capture), false, 'task runner should not be invoked without a workspace mount');
  const missingWorkspaceOutcome = JSON.parse(missingWorkspaceResult.stdout);
  assert.equal(missingWorkspaceOutcome.status, 'failed');
  assert.equal(missingWorkspaceOutcome.failure_classification, 'execution_failed');
  assert.equal(missingWorkspaceOutcome.diagnostics[0].class, 'codebox.preflight.missing_workspace');
  assert.equal(missingWorkspaceOutcome.diagnostics[0].data.repo, 'a8c-intelligence');
  assert.equal(missingWorkspaceOutcome.diagnostics[0].data.task_kind, 'repo-cooking');
  assert.equal(missingWorkspaceOutcome.diagnostics[0].data.mounts_count, 0);
  assert.equal(missingWorkspaceOutcome.metadata.codebox.missing_workspace, true);

  const claudeCodeBaseRequest = {
    ...request,
    task_id: 'claude-code-preflight-task',
    executor: {
      backend: 'codebox',
      model: 'claude-sonnet-4-6',
      config: {
        provider: 'claude-code',
        provider_plugin_paths: ['/components/ai-provider-for-claude-code'],
        wp_codebox_bin: '/bin/should-not-launch-wp-codebox',
      },
    },
  };
  const claudeMissingValueResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify(claudeCodeBaseRequest),
    env: fixtureEnv({
      [claudeCodeRefreshTokenEnv]: '',
    }),
  });
  assert.equal(claudeMissingValueResult.status, 1, claudeMissingValueResult.stderr || claudeMissingValueResult.stdout);
  const claudeMissingValueOutcome = JSON.parse(claudeMissingValueResult.stdout);
  assert.equal(claudeMissingValueOutcome.status, 'failed');
  assert.equal(claudeMissingValueOutcome.failure_classification, 'provider');
  assert.equal(claudeMissingValueOutcome.diagnostics[0].class, 'codebox.preflight.claude_code_auth');
  assert.deepEqual(claudeMissingValueOutcome.diagnostics[0].data.missing_env, [claudeCodeRefreshTokenEnv]);
  assert(!JSON.stringify(claudeMissingValueOutcome).includes('claude-refresh-token-value'));

  const claudeMissingMappingResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify({
      ...claudeCodeBaseRequest,
      task_id: 'claude-code-missing-mapping-task',
      executor: {
        ...claudeCodeBaseRequest.executor,
        secret_env: ['AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN'],
      },
    }),
    env: fixtureEnv({
      AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN: 'claude-access-token-value',
      [claudeCodeRefreshTokenEnv]: 'claude-refresh-token-value',
    }),
  });
  assert.equal(claudeMissingMappingResult.status, 1, claudeMissingMappingResult.stderr || claudeMissingMappingResult.stdout);
  const claudeMissingMappingOutcome = JSON.parse(claudeMissingMappingResult.stdout);
  assert.equal(claudeMissingMappingOutcome.status, 'failed');
  assert.equal(claudeMissingMappingOutcome.failure_classification, 'provider');
  assert.equal(claudeMissingMappingOutcome.diagnostics[0].class, 'codebox.preflight.claude_code_auth');
  assert.deepEqual(claudeMissingMappingOutcome.diagnostics[0].data.missing_env, [claudeCodeRefreshTokenEnv]);
  assert(!JSON.stringify(claudeMissingMappingOutcome).includes('claude-access-token-value'));
  assert(!JSON.stringify(claudeMissingMappingOutcome).includes('claude-refresh-token-value'));
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}

console.log('Codebox agent task executor smoke passed');
