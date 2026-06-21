'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const {
  agentTaskOutcomeFromCodeboxResult,
  codeboxTaskRequestFromAgentTaskRequest,
  missingRequiredSecretEnvMapping,
  missingRequiredSecretEnvValues,
  providerContract,
  providerPreflightManifest,
  providerRequiredSecretEnv,
  providerSecretEnv,
} = require('../../agent-runtimes/wp-codebox');
const {
  AGENT_TASK_FAILURE_CLASSIFICATIONS,
  AGENT_TASK_OUTCOME_STATUSES,
  AGENT_TASK_REDACTED_METADATA_KEYS,
} = require('../../runtime-agent-ci/lib/agent-task-provider-contract');

const fixtureCodeboxCoreModule = path.join(__dirname, 'fixtures', 'wp-codebox-core-agent-task-normalizer.mjs');
const wpCodeboxRuntimeRoot = path.join(__dirname, '..', '..', 'agent-runtimes', 'wp-codebox');
const wpCodeboxRuntimeExecutor = path.join(wpCodeboxRuntimeRoot, 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs');
const codexSecretEnv = [
  'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
  'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
  'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
];
const claudeCodeSecretEnv = [
  'AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN',
  'AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN',
  'AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT',
];
const claudeCodeRefreshTokenEnv = 'AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN';
const repoLoopCapabilities = [
  'tool:example/run-agent-bundle',
  'tool:github_pull_request_publish',
  'ability:example/run-agent-bundle',
  'ability:github_pull_request_publish',
];

function exampleAgentCiCodeboxExecutorConfig(config = {}) {
  const profileId = 'example-agent-ci';
  return {
    ...config,
    runtime_profile: config.runtime_profile || profileId,
    runtime_profiles: {
      ...(config.runtime_profiles || {}),
      [profileId]: {
        id: profileId,
        runtime_task_ability: 'example/run-agent-bundle',
        runtime_bundle_ability: 'example/run-agent-bundle',
        ability_requirements: ['example/run-agent-bundle'],
      },
    },
  };
}

function secretEnvRequirementForProvider(contract, provider) {
  return contract.secret_env_requirements.find((requirement) => (
    requirement.when.any.some((selector) => selector.path === 'executor.config.provider' && selector.equals === provider)
  ));
}

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
    success: true,
    status: 'completed',
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

function writeCompletedJsonFailingTaskRunner(root) {
  const fixture = path.join(root, 'completed-json-failing-task-runner.cjs');
  fs.writeFileSync(fixture, `#!/usr/bin/env node
'use strict';
process.stdout.write(JSON.stringify({
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  summary: 'Semantic output was produced before provider exit failure.',
  outputs: { issue_url: 'https://github.com/example/repo/issues/456' },
  artifacts: [{ id: 'semantic-artifact', kind: 'codebox-patch', path: '/tmp/semantic.patch' }],
  session: { id: 'sandbox-session-failed-exit', status: 'completed' }
}));
process.exit(7);
`);
  fs.chmodSync(fixture, 0o755);
  return fixture;
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
        engine_data: { example_agent: { pr_url: 'https://github.com/example-org/example-repo/pull/123' } }
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
            engine_data: { example_agent: { pr_url: 'https://github.com/example-org/example-repo/pull/123' } }
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
  const bundle = path.join(root, 'example-agent');
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
        kind: 'bundled-library',
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
const codexSecretEnvSources = {
  AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: {
    source: 'json-file',
    path: '~/.codex/auth.json',
    field: 'tokens.access_token',
  },
  AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: {
    source: 'json-file',
    path: '~/.codex/auth.json',
    field: 'tokens.refresh_token',
  },
  AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: {
    source: 'json-file-jwt-expiration',
    path: '~/.codex/auth.json',
    field: 'tokens.access_token',
  },
  AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: {
    source: 'json-file',
    path: '~/.codex/auth.json',
    field: 'tokens.account_id',
  },
  AI_PROVIDER_OPENAI_CODEX_FEDRAMP: {
    source: 'json-file',
    path: '~/.codex/auth.json',
    field: 'tokens.fedramp',
    value: 'false',
  },
};
assert.equal(provider.id, 'wordpress.codebox-agent-task-executor');
assert.equal(provider.label, 'WP Codebox agent task executor');
assert.equal(provider.backend, 'codebox');
assert.equal(provider.command, 'node {{runtime_path}}/scripts/agent/homeboy-codebox-agent-task-executor.cjs');
assert.equal(provider.request_schema, 'homeboy/agent-task-request/v1');
assert.equal(provider.outcome_schema, 'homeboy/agent-task-outcome/v1');
assert.deepEqual(provider.request_required_fields, ['schema', 'task_id', 'executor.backend', 'instructions']);
assert.deepEqual(provider.outcome_statuses, AGENT_TASK_OUTCOME_STATUSES);
assert.deepEqual(provider.failure_classifications, AGENT_TASK_FAILURE_CLASSIFICATIONS);
assert.deepEqual(provider.redacted_metadata_keys, AGENT_TASK_REDACTED_METADATA_KEYS);
assert.deepEqual(secretEnvRequirementForProvider(provider, 'codex').env, codexSecretEnv);
assert.deepEqual(secretEnvRequirementForProvider(provider, 'codex').when, {
  any: [
    { path: 'executor.config.provider', equals: 'codex' },
    { path: 'executor.provider', equals: 'codex' },
    { path: 'provider', equals: 'codex' },
  ],
});
assert.deepEqual(secretEnvRequirementForProvider(provider, 'openai').env, ['OPENAI_API_KEY']);
assert.deepEqual(secretEnvRequirementForProvider(provider, 'claude-code').env, claudeCodeSecretEnv);
assert.deepEqual(provider.workspace_materialization, { cwd: 'git_checkout' });
assert.deepEqual(provider.provider_defaults, {
  openai: {
    secret_env: ['OPENAI_API_KEY'],
  },
  codex: {
    secret_env: [
      'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
      'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
      'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
      'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
      'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
    ],
    secret_env_sources: codexSecretEnvSources,
  },
  'claude-code': {
    secret_env: claudeCodeSecretEnv,
    secret_env_sources: {
      AI_PROVIDER_CLAUDE_CODE_ACCESS_TOKEN: {
        source: 'json-file',
        path: '~/.local/share/opencode/auth.json',
        field: 'anthropic.access',
      },
      AI_PROVIDER_CLAUDE_CODE_REFRESH_TOKEN: {
        source: 'json-file',
        path: '~/.local/share/opencode/auth.json',
        field: 'anthropic.refresh',
      },
      AI_PROVIDER_CLAUDE_CODE_EXPIRES_AT: {
        source: 'json-file',
        path: '~/.local/share/opencode/auth.json',
        field: 'anthropic.expires',
      },
    },
  },
});
assert.deepEqual(providerRequiredSecretEnv('codex'), codexSecretEnv.slice(0, 4));
assert.deepEqual(providerSecretEnv('codex'), codexSecretEnv);
assert.deepEqual(providerRequiredSecretEnv('claude-code'), [claudeCodeRefreshTokenEnv]);
assert.equal(providerPreflightManifest('codex').refresh_hook, 'codex-oauth-refresh');
assert.equal(providerPreflightManifest('codex').provider_plugin_validation.diagnostic_class, 'codebox.preflight.codex_provider_plugin_path');
assert.deepEqual(missingRequiredSecretEnvMapping({ secret_env: codexSecretEnv.slice(0, 3) }, 'codex'), ['AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID']);
assert.deepEqual(missingRequiredSecretEnvValues('claude-code', {}), [claudeCodeRefreshTokenEnv]);
assert.deepEqual(provider.role_aliases, {
  artifact_roles: {
    artifact_bundle: ['codebox-artifact-bundle', 'artifact-bundle', 'codebox-artifact-directory', 'codebox-session-artifacts'],
    changed_files: ['codebox-changed-files'],
    patch: ['codebox-patch'],
    transcript: ['codebox-transcript', 'agent-runtime-transcript', 'agent-runtime-transcript-summary'],
    runtime_log: ['codebox-runtime-log', 'codebox-recipe-startup-log'],
    command_log: ['codebox-command-log'],
    typed_artifact: ['typed-bundle-output'],
    replay_bundle: ['agent-runtime-replay-bundle'],
    pull_request: ['agent-runtime-pull-request'],
    probe_result: ['codebox-recipe-probe-json', 'recipe-probe-result'],
    screenshot: ['codebox-recipe-screenshot'],
    side_effects: ['codebox-recipe-fake-side-effects'],
    preflight_evidence: ['codebox-command-evidence', 'codebox-agent-task-input'],
  },
  artifact_kinds: {
    patch: ['codebox-patch'],
  },
  artifact_filenames: {
    preflight_evidence: ['homeboy-codebox-task-runner.json'],
  },
  outputs: {
    provider_run_result: ['codebox_run_result'],
  },
  metadata: {
    provider_run_result: ['codebox_run_result'],
  },
});
assert.equal(provider.status, 'active');
assert.equal(provider.integration_contract, 'homeboy-wordpress-agent-task/v1');
assert.equal(provider.capabilities.includes('browser_runtime'), true);
assert.equal(provider.capabilities.includes('workspace_tools'), true);
assert.equal(provider.capabilities.includes('patch_artifacts'), true);
assert.equal(provider.capabilities.includes('cleanup_observability'), true);
assert.equal(provider.capabilities.includes('ability_execution'), true);
assert.equal(provider.capabilities.includes('agent_bundle_execution'), true);
assert.equal(provider.capabilities.includes('workflow_execution'), true);
assert.equal(provider.capabilities.includes('typed_bundle_outputs'), true);
assert.equal(provider.capabilities.includes('external_recipe_packs'), true);
assert.equal(provider.capabilities.includes('recipe_probe_artifacts'), true);
assert.deepEqual(provider.runtime_execution_contracts.bundle, {
  ability_field: 'runtime_bundle_ability',
  required_capabilities: ['agent_bundle_execution'],
});
for (const capability of repoLoopCapabilities) {
  assert.equal(provider.capabilities.includes(capability), false);
}
const manifest = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'wordpress.json'), 'utf8'));
assert.equal(manifest.agent_task_executors, undefined);
assert.equal(manifest.agent_runtimes, undefined);
assert.equal(manifest.agent_task.default_backend, undefined);
assert.equal(manifest.agent_task.runtime_requirements.integration_contract, 'homeboy-wordpress-agent-task/v1');
const runtimeManifest = JSON.parse(fs.readFileSync(path.join(wpCodeboxRuntimeRoot, 'wp-codebox.json'), 'utf8'));
const manifestProvider = runtimeManifest.agent_task_executors.find((executor) => executor.id === provider.id);
assert.deepEqual(manifestProvider, providerContract());
assert.deepEqual(secretEnvRequirementForProvider(manifestProvider, 'codex').env, codexSecretEnv);
for (const capability of repoLoopCapabilities) {
  assert.equal(manifestProvider.capabilities.includes(capability), false);
}
assert.equal(provider.capabilities.includes('tool:wpsg_materialize_packet'), false);
assert.equal(provider.capabilities.includes('ability:wpsg_materialize_packet'), false);
assert.deepEqual(provider.runtime_gap_trackers.map((tracker) => tracker.gap), ['runtime-profile-normalizer', 'typed-artifact-dto-normalizer']);

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
  kind: 'bundled-library',
  library: 'php-ai-client',
  source: '/components/php-ai-client',
  target: '/wordpress/wp-includes/php-ai-client',
  metadata: { component: 'php-ai-client', ref: 'custom-provider-auth' },
}]);
assert.deepEqual(codeboxRequest.runtime_requirements, {
  schema: 'wp-codebox/runtime-profile/v1',
  runtime_overlays: codeboxRequest.runtime_overlays,
  provider_plugins: [{ path: '/providers/openai' }],
  upstream_primitive_requirements: [{
    schema: 'wp-codebox/upstream-primitive-requirement/v1',
    id: 'parent-tool-bridge',
    owner: 'wp-codebox',
    primitive_schema: 'wp-codebox/parent-tool-bridge/v1',
    status: 'required-upstream-primitive',
    adapter_behavior: 'declare_requirement_only',
    requirement: 'Expose parent-owned tools inside the sandbox through a Codebox-owned bridge component declared by the public parent-tool-bridge contract.',
  }],
});
const legacyOverlayNameRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'invalid-runtime-overlay-type-task-123',
  executor: {
    ...request.executor,
    config: {
      ...request.executor.config,
      runtime_overlays: [{ type: 'bundled-library', library: 'php-ai-client', source: '/components/php-ai-client' }],
    },
  },
});
assert.deepEqual(legacyOverlayNameRequest.runtime_overlays, [{ type: 'bundled-library', library: 'php-ai-client', source: '/components/php-ai-client' }]);
assert.deepEqual(codeboxRequest.runtime_env, {});
assert.deepEqual(codeboxRequest.runtime_state_mounts, []);
assert.deepEqual(codeboxRequest.runtime_config_mounts, []);
assert.deepEqual(codeboxRequest.secret_env, ['OPENAI_API_KEY']);

const providerDefaultSecretRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'provider-default-secret-task-123',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'claude-code',
      model: 'opus-4.7',
    },
  },
});
assert.deepEqual(providerDefaultSecretRequest.secret_env, provider.provider_defaults['claude-code'].secret_env);

const codexDefaultSecretRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'codex-default-secret-task-123',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'codex',
      model: 'gpt-5.5',
    },
  },
});
assert.deepEqual(codexDefaultSecretRequest.secret_env, provider.provider_defaults.codex.secret_env);
assert.equal(codeboxRequest.max_turns, 8);
assert.equal(codeboxRequest.task_timeout_seconds, 120);
assert.equal(codeboxRequest.expected_artifacts[0], 'screenshot');
assert.equal(codeboxRequest.orchestrator.agent_task_id, 'task-123');
assert.equal(codeboxRequest.context.audit_findings[0].id, 'finding-1');
assert.deepEqual(codeboxRequest.agent_bundle, {});

const artifactDeclarationRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'artifact-declaration-task-123',
  artifact_declarations: [{
    schema: 'homeboy/agent-task-artifact-declaration/v1',
    name: 'analysis_report',
    type: 'AnalysisReport',
    artifact_schema: 'example/analysis-report/v1',
    path: 'artifacts/analysis-report.json',
    required: true,
  }],
});
assert.equal(artifactDeclarationRequest.artifact_declarations[0].schema, 'wp-codebox/artifact-declaration/v1');
assert.equal(artifactDeclarationRequest.artifact_declarations[0].name, 'analysis_report');
assert.equal(artifactDeclarationRequest.artifact_declarations[0].path, 'artifacts/analysis-report.json');
assert.equal(artifactDeclarationRequest.artifact_declarations[0].required, true);

const legacyArtifactDeclarationRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'legacy-artifact-declaration-task-123',
  artifactDeclarations: [{
    name: 'legacy_report',
    kind: 'LegacyReport',
    contentSchema: 'example/legacy-report/v1',
  }],
});
assert.equal(legacyArtifactDeclarationRequest.artifact_declarations[0].name, 'legacy_report');
assert.equal(legacyArtifactDeclarationRequest.artifact_declarations[0].type, 'LegacyReport');
assert.equal(legacyArtifactDeclarationRequest.artifact_declarations[0].artifact_schema, 'example/legacy-report/v1');

const codeboxRequestWithAbilityTools = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'ability-tools-task-123',
  executor: {
    ...request.executor,
    config: {
      ...request.executor.config,
      ability_tools: [{ name: 'example_materialize_packet', ability: 'example/materialize-packet' }],
    },
  },
});
assert.deepEqual(codeboxRequestWithAbilityTools.ability_tools, [{ name: 'example_materialize_packet', ability: 'example/materialize-packet' }]);

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
assert.deepEqual(claudeCodeDefaultSecretEnvRequest.secret_env, claudeCodeSecretEnv);

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
      agent_bundles: [{ source: '/workspace/bundles/canary-agent', slug: 'canary-agent' }],
      structured_artifacts: [{
        schema: 'wp-codebox/structured-artifact/v1',
        name: 'concept_packet',
        type: 'ConceptPacket',
        payload: { title: 'Canary concept' },
        metadata: {},
        provenance: { source: 'test' },
      }],
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
assert.deepEqual(runtimeTaskRequest.agent_bundles, [{ source: '/workspace/bundles/canary-agent', slug: 'canary-agent' }]);
assert.equal(runtimeTaskRequest.structured_artifacts[0].name, 'concept_packet');
assert.equal(runtimeTaskRequest.workspaces[0].target, '/workspace/codebox-canary');

const runtimeTaskProviderDefaultRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'runtime-task-provider-defaults-123',
  executor: {
    backend: 'codebox',
    model: 'openai/gpt-5.5',
    config: {
      provider: 'opencode',
      agent_bundles: [{ source: '/workspace/bundles/canary-agent', slug: 'canary-agent' }],
    },
  },
  inputs: {
    runtime_task: {
      ability: 'runtime/run-agent-bundle',
      input: { source: '/workspace/bundles/canary-agent' },
    },
  },
});
assert.equal(runtimeTaskProviderDefaultRequest.runtime_task.input.provider, 'opencode', 'agent bundle runtime tasks inherit the selected provider');
assert.equal(runtimeTaskProviderDefaultRequest.runtime_task.input.model, 'openai/gpt-5.5', 'agent bundle runtime tasks inherit the selected model');

const runtimeTaskExplicitProviderRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'runtime-task-explicit-provider-123',
  executor: {
    backend: 'codebox',
    model: 'openai/gpt-5.5',
    config: {
      provider: 'opencode',
      agent_bundles: [{ source: '/workspace/bundles/canary-agent', slug: 'canary-agent' }],
    },
  },
  inputs: {
    runtime_task: {
      ability: 'runtime/run-agent-bundle',
      input: { source: '/workspace/bundles/canary-agent', provider: 'explicit-provider', model: 'explicit-model' },
    },
  },
});
assert.equal(runtimeTaskExplicitProviderRequest.runtime_task.input.provider, 'explicit-provider', 'explicit runtime task provider wins');
assert.equal(runtimeTaskExplicitProviderRequest.runtime_task.input.model, 'explicit-model', 'explicit runtime task model wins');

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
      component_contracts: [{ slug: 'example-repo', path: '/workspace/example-repo', activate: true }],
      engine_data_outputs: {
        validation_result: 'metadata.artifacts.ImportValidationResult',
      },
    },
  },
});
assert.equal(abilityBridgeRequest.runtime_task.ability, 'example/validate-artifact');
assert.deepEqual(abilityBridgeRequest.runtime_task.input, { artifact: { slug: 'example-site' }, report: '/artifacts/import-report.json' });
assert.equal(abilityBridgeRequest.parent_request.executor.config.output_mappings.validation_result, 'result.import_validation_result');
assert.deepEqual(abilityBridgeRequest.component_contracts, [{ slug: 'example-repo', path: '/workspace/example-repo', activate: true }]);

const topLevelComponentContractsRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'top-level-component-contracts-task-123',
  component_contracts: [{ slug: 'domain-component', path: '/workspace/domain-component', activate: true }],
  executor: {
    backend: 'codebox',
    config: {
      component_contracts: [{ slug: 'config-component', path: '/workspace/config-component', activate: false }],
    },
  },
});
assert.deepEqual(topLevelComponentContractsRequest.component_contracts, [
  { slug: 'domain-component', path: '/workspace/domain-component', activate: true },
  { slug: 'config-component', path: '/workspace/config-component', activate: false },
]);
assert.deepEqual(topLevelComponentContractsRequest.runtime_requirements.component_contracts, topLevelComponentContractsRequest.component_contracts);
assert.deepEqual(topLevelComponentContractsRequest.runtime_requirements.extra_plugins, topLevelComponentContractsRequest.component_contracts);

const genericRuntimeEnv = {
  GENERIC_PROVIDER_CONFIG: '/runtime/provider/config.json',
  XDG_DATA_HOME: '/runtime/provider/data',
};
const genericRuntimeStateMounts = [{
  source: '/host/provider/state.json',
  target: '/runtime/provider/state.json',
  mode: 'readonly',
  metadata: { purpose: 'provider-state' },
}];
const genericRuntimeConfigMounts = [{
  source: '/host/provider/config.json',
  target: '/runtime/provider/config.json',
  mode: 'readonly',
  metadata: { purpose: 'provider-config' },
}];
const genericProviderRuntimeRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'generic-runtime-env-task-123',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'fixture-provider',
      runtime_env: genericRuntimeEnv,
      runtime_state_mounts: genericRuntimeStateMounts,
      runtime_config_mounts: genericRuntimeConfigMounts,
      provider_plugin_paths: ['/providers/fixture-provider'],
      runtime_overlays: [{ kind: 'fixture-overlay', source: '/overlays/fixture' }],
      runtime_overlay_profiles: ['fixture-profile'],
      secret_env: ['FIXTURE_PROVIDER_SECRET'],
    },
  },
});
assert.equal(genericProviderRuntimeRequest.provider, 'fixture-provider');
assert.deepEqual(genericProviderRuntimeRequest.runtime_env, genericRuntimeEnv);
assert.deepEqual(genericProviderRuntimeRequest.runtime_state_mounts, genericRuntimeStateMounts);
assert.deepEqual(genericProviderRuntimeRequest.runtime_config_mounts, genericRuntimeConfigMounts);
assert.deepEqual(genericProviderRuntimeRequest.provider_plugin_paths, ['/providers/fixture-provider']);
assert.deepEqual(genericProviderRuntimeRequest.runtime_overlays, [{ kind: 'fixture-overlay', source: '/overlays/fixture' }]);
assert.deepEqual(genericProviderRuntimeRequest.runtime_requirements, {
  schema: 'wp-codebox/runtime-profile/v1',
  runtime_overlays: [{ kind: 'fixture-overlay', source: '/overlays/fixture' }],
  env: genericRuntimeEnv,
  provider_plugins: [{ path: '/providers/fixture-provider' }],
  runtime_state_mounts: genericRuntimeStateMounts,
  runtime_config_mounts: genericRuntimeConfigMounts,
  upstream_primitive_requirements: [{
    schema: 'wp-codebox/upstream-primitive-requirement/v1',
    id: 'parent-tool-bridge',
    owner: 'wp-codebox',
    primitive_schema: 'wp-codebox/parent-tool-bridge/v1',
    status: 'required-upstream-primitive',
    adapter_behavior: 'declare_requirement_only',
    requirement: 'Expose parent-owned tools inside the sandbox through a Codebox-owned bridge component declared by the public parent-tool-bridge contract.',
  }],
});
assert.deepEqual(genericProviderRuntimeRequest.runtime_overlay_profiles, ['fixture-profile']);
assert.deepEqual(genericProviderRuntimeRequest.secret_env, ['FIXTURE_PROVIDER_SECRET']);

const codeboxOwnedParentToolBridgeRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'codebox-owned-parent-tool-bridge-task-123',
  executor: {
    backend: 'codebox',
    config: {
      runtime_profile: {
        schema: 'wp-codebox/runtime-profile/v1',
        parent_tool_bridge: {
          schema: 'wp-codebox/parent-tool-bridge/v1',
          mode: 'codebox-owned',
        },
      },
    },
  },
});
assert.equal(codeboxOwnedParentToolBridgeRequest.runtime_requirements.parent_tool_bridge.mode, 'codebox-owned');
assert.equal(codeboxOwnedParentToolBridgeRequest.runtime_requirements.homeboy_parent_tool_bridge, undefined);

const optionsRuntimeRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'options-runtime-env-task-123',
  executor: {
    backend: 'codebox',
    config: { provider: 'another-fixture-provider' },
  },
}, {
  runtimeEnv: { OPTIONS_PROVIDER_HOME: '/runtime/options-provider' },
  runtimeStateMounts: [{ source: '/host/options-state', target: '/runtime/options-state', mode: 'readonly' }],
  runtimeConfigMounts: [{ source: '/host/options-config', target: '/runtime/options-config', mode: 'readonly' }],
});
assert.equal(optionsRuntimeRequest.provider, 'another-fixture-provider');
assert.deepEqual(optionsRuntimeRequest.runtime_env, { OPTIONS_PROVIDER_HOME: '/runtime/options-provider' });
assert.deepEqual(optionsRuntimeRequest.runtime_state_mounts, [{ source: '/host/options-state', target: '/runtime/options-state', mode: 'readonly' }]);
assert.deepEqual(optionsRuntimeRequest.runtime_config_mounts, [{ source: '/host/options-config', target: '/runtime/options-config', mode: 'readonly' }]);

const previousHomeboySettingsJson = process.env.HOMEBOY_SETTINGS_JSON;
process.env.HOMEBOY_SETTINGS_JSON = JSON.stringify({
  wp_codebox_provider: 'settings-fixture-provider',
  wp_codebox_runtime_env: { SETTINGS_PROVIDER_HOME: '/runtime/settings-provider' },
  wp_codebox_runtime_state_mounts: [{ source: '/host/settings-state', target: '/runtime/settings-state', mode: 'readonly' }],
  wp_codebox_runtime_config_mounts: [{ source: '/host/settings-config', target: '/runtime/settings-config', mode: 'readonly' }],
});
try {
  const settingsRuntimeRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'settings-runtime-env-task-123',
    executor: {
      backend: 'codebox',
      config: {},
    },
  });
  assert.equal(settingsRuntimeRequest.provider, 'settings-fixture-provider');
  assert.deepEqual(settingsRuntimeRequest.runtime_env, { SETTINGS_PROVIDER_HOME: '/runtime/settings-provider' });
  assert.deepEqual(settingsRuntimeRequest.runtime_state_mounts, [{ source: '/host/settings-state', target: '/runtime/settings-state', mode: 'readonly' }]);
  assert.deepEqual(settingsRuntimeRequest.runtime_config_mounts, [{ source: '/host/settings-config', target: '/runtime/settings-config', mode: 'readonly' }]);
} finally {
  if (previousHomeboySettingsJson === undefined) {
    delete process.env.HOMEBOY_SETTINGS_JSON;
  } else {
    process.env.HOMEBOY_SETTINGS_JSON = previousHomeboySettingsJson;
  }
}

assert.equal(fs.readFileSync(path.join(wpCodeboxRuntimeRoot, 'lib', 'codebox-agent-task-executor.js'), 'utf8').includes('opencode'), false);

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

const roleMatrixRecipeRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'role-matrix-recipe-task-123',
  executor: {
    backend: 'codebox',
    model: 'gpt-5.5',
    config: {
      recipe_pack: 'example-codebox-recipes',
      recipe: 'role-matrix-runtime',
      runtime_profile: {
        role_matrix: [
          { name: 'admin', role: 'administrator', capabilities: ['manage_options'] },
          { name: 'editor', role: 'editor', capabilities: ['edit_posts'], session: 'editor-rest' },
        ],
      },
    },
  },
  inputs: {
    recipe_inputs: {
      fixture: 'role-matrix',
      fixtureUsers: [{ name: 'admin', username: 'explicit-admin', role: 'administrator' }],
      userSessions: [{ name: 'admin-session', user: 'admin' }],
    },
  },
});
assert.deepEqual(roleMatrixRecipeRequest.recipe.inputs, {
  fixture: 'role-matrix',
  fixtureUsers: [
    { name: 'admin', username: 'explicit-admin', role: 'administrator' },
    { name: 'editor', username: 'fixture-editor', role: 'editor', metadata: { capabilities: ['edit_posts'] } },
  ],
  userSessions: [
    { name: 'admin-session', user: 'admin' },
    { name: 'editor-rest', user: 'editor', metadata: { role: 'editor', capabilities: ['edit_posts'] } },
  ],
});

const capabilityMatrixRecipeRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'capability-matrix-recipe-task-123',
  executor: {
    backend: 'codebox',
    model: 'gpt-5.5',
    config: {
      recipe_pack: 'example-codebox-recipes',
      recipe: 'capability-matrix-runtime',
      runtime_requirements: {
        capability_matrix: {
          shop_manager: ['manage_woocommerce', 'view_woocommerce_reports'],
        },
      },
    },
  },
});
assert.deepEqual(capabilityMatrixRecipeRequest.recipe.inputs.fixtureUsers, [{
  name: 'shop_manager',
  username: 'fixture-shop_manager',
  role: 'shop_manager',
  metadata: { capabilities: ['manage_woocommerce', 'view_woocommerce_reports'] },
}]);
assert.deepEqual(capabilityMatrixRecipeRequest.recipe.inputs.userSessions, [{
  name: 'shop_manager-session',
  user: 'shop_manager',
  metadata: { role: 'shop_manager', capabilities: ['manage_woocommerce', 'view_woocommerce_reports'] },
}]);

const codexAgentRequest = {
  ...request,
  task_id: 'codex-task-123',
  executor: {
    backend: 'codebox',
    model: 'gpt-5.5',
    config: exampleAgentCiCodeboxExecutorConfig({
      provider: 'codex',
      provider_plugin_paths: ['/components/ai-provider-for-openai'],
      secret_env: [
        'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
        'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
        'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
        'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
        'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
      ],
      runtime_component_paths: {
        agent_runtime: '/components/example-runtime',
        agent_runtime_tools: '/components/example-runtime-tools',
      },
      homeboy: '/components/homeboy',
      homeboy_extensions: '/components/homeboy-extensions',
      wp_codebox_bin: '/bin/wp-codebox',
      max_turns: 8,
    }),
  },
};
const codexRequest = codeboxTaskRequestFromAgentTaskRequest(codexAgentRequest);
assert.equal(Object.hasOwn(codexRequest, 'agent'), false);
assert.equal(codexRequest.mode, 'sandbox');
assert.equal(codexRequest.provider, 'codex');
assert.equal(codexRequest.model, 'gpt-5.5');
assert.deepEqual(codexRequest.provider_plugin_paths, ['/components/ai-provider-for-openai']);
assert.equal(codexRequest.runtime_component_paths.agent_runtime, '/components/example-runtime');
assert.equal(codexRequest.runtime_component_paths.agent_runtime_tools, '/components/example-runtime-tools');
assert.equal(Object.hasOwn(codexRequest.runtime_component_paths, 'agents_api'), false);
assert.equal(Object.hasOwn(codexRequest, 'agents_api_path'), false);
assert.equal(Object.hasOwn(codexRequest, 'data_machine_path'), false);
assert.equal(Object.hasOwn(codexRequest, 'data_machine_code_path'), false);
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

const workflowStyleConfigRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'runtime-contract-task-123',
  executor: {
    backend: 'codebox',
    model: 'gpt-5.5',
    config: exampleAgentCiCodeboxExecutorConfig({
      provider: 'codex',
      runtime_bin: '/bin/wp-codebox-runtime',
      wordpress_runtime_version: 'beta',
      runtime_mounts: [{
        type: 'file',
        source: '/host/driver.php',
        target: '/wordpress/wp-content/plugins/driver/driver.php',
        mode: 'readonly',
      }],
      runtime_component_paths: {
        runtime: '/components/wp-codebox-runtime-plugin',
        agent_runtime: '/components/example-runtime',
        agent_runtime_tools: '/components/example-runtime-tools',
      },
    }),
  },
});
assert.equal(workflowStyleConfigRequest.wp_codebox_bin, '/bin/wp-codebox-runtime');
assert.equal(workflowStyleConfigRequest.wp, 'beta');
assert.deepEqual(workflowStyleConfigRequest.mounts, [{
  type: 'file',
  source: '/host/driver.php',
  target: '/wordpress/wp-content/plugins/driver/driver.php',
  mode: 'readonly',
}]);
assert.equal(workflowStyleConfigRequest.runtime_component_paths.runtime, '/components/wp-codebox-runtime-plugin');
assert.equal(Object.hasOwn(workflowStyleConfigRequest.runtime_component_paths, 'agents_api'), false);
assert.equal(workflowStyleConfigRequest.runtime_component_paths.agent_runtime, '/components/example-runtime');
assert.equal(workflowStyleConfigRequest.runtime_component_paths.agent_runtime_tools, '/components/example-runtime-tools');
assert.equal(Object.hasOwn(workflowStyleConfigRequest.runtime_component_paths, 'data_machine'), false);
assert.equal(Object.hasOwn(workflowStyleConfigRequest.runtime_component_paths, 'data_machine_code'), false);

const deprecatedWordPressRuntimeVersionRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'runtime-contract-task-deprecated-wordpress-version',
  executor: {
    backend: 'codebox',
    config: exampleAgentCiCodeboxExecutorConfig({
      provider: 'codex',
      wp_codebox_wordpress_version: '6.9',
    }),
  },
});
assert.equal(deprecatedWordPressRuntimeVersionRequest.wp, '6.9');

const defaultsRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-agent-task-defaults-'));
try {
  const workspaceRoot = path.join(defaultsRoot, 'target-repo@issue-1161');
  const runtimePath = path.join(defaultsRoot, 'example-runtime');
  const bundledAgentsApiPath = path.join(runtimePath, 'vendor', 'wordpress', 'agents-api');
  const alternateBundledAgentsApiPath = path.join(runtimePath, 'vendor', 'automattic', 'agents-api');
  const runtimeToolsPath = path.join(defaultsRoot, 'example-runtime-tools');
  const staleStandaloneAgentsApiPath = path.join(defaultsRoot, 'agents-api');
  const providerPath = path.join(defaultsRoot, 'ai-provider-for-openai');
  const phpAiClientPath = path.join(defaultsRoot, 'php-ai-client');
  for (const directory of [workspaceRoot, bundledAgentsApiPath, runtimeToolsPath, staleStandaloneAgentsApiPath, providerPath, phpAiClientPath]) {
    fs.mkdirSync(directory, { recursive: true });
  }

  const defaultedRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'default-runtime-stack-task-123',
    executor: {
      backend: 'codebox',
      config: exampleAgentCiCodeboxExecutorConfig({ provider: 'codex' }),
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: {},
  });
  assert.deepEqual(defaultedRequest.runtime_component_paths, {});
  assert.deepEqual(defaultedRequest.provider_plugin_paths, []);
  assert.deepEqual(defaultedRequest.runtime_overlay_profiles, []);
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
  assert(!JSON.stringify(defaultedRequest).includes(providerPath));
  assert(!JSON.stringify(defaultedRequest).includes(phpAiClientPath));
  assert(!JSON.stringify(defaultedRequest).includes(staleStandaloneAgentsApiPath));
  assert(!JSON.stringify(defaultedRequest).includes(alternateBundledAgentsApiPath));

  const configuredDefaultProviderRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'configured-provider-default-task-123',
    executor: {
      backend: 'codebox',
      config: {},
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: {
      wp_codebox_default_provider: 'codex',
      wp_codebox_provider_defaults: {
        codex: {
          model: 'gpt-5.5',
          secret_env: codexSecretEnv,
        },
      },
    },
  });
  assert.equal(configuredDefaultProviderRequest.provider, 'codex');
  assert.equal(configuredDefaultProviderRequest.model, 'gpt-5.5');
  assert.deepEqual(configuredDefaultProviderRequest.provider_plugin_paths, []);
  assert.deepEqual(configuredDefaultProviderRequest.runtime_overlay_profiles, []);
  assert.deepEqual(configuredDefaultProviderRequest.runtime_overlays, []);
  assert.deepEqual(configuredDefaultProviderRequest.secret_env, codexSecretEnv);

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
  assert.equal(openAiDefaultedRequest.provider, '');
  assert.equal(openAiDefaultedRequest.model, 'gpt-5.5');
  assert.deepEqual(openAiDefaultedRequest.secret_env, []);

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
  assert.equal(bareOpenAiDefaultedRequest.provider, '');
  assert.equal(bareOpenAiDefaultedRequest.model, '');
  assert.deepEqual(bareOpenAiDefaultedRequest.secret_env, []);
  // A bare repo workspace defaults to a read-write mount, so coding-capable
  // workspace tools must be exposed alongside the read-only inspection tools.
  const writableWorkspaceTools = [
    'workspace_ls',
    'workspace_read',
    'workspace_git_status',
    'workspace_run_runner_command',
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
      config: exampleAgentCiCodeboxExecutorConfig({ provider: 'codex' }),
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: {},
  });
  assert.deepEqual(alternateDefaultedRequest.runtime_component_paths, {});

  const explicitProviderPath = path.join(defaultsRoot, 'provider-plugin');
  fs.mkdirSync(explicitProviderPath, { recursive: true });
  const explicitProviderRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'explicit-provider-path-task-123',
    executor: {
      backend: 'codebox',
      config: { provider: 'codex' },
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: { wp_codebox_provider_plugin_paths: { codex: [explicitProviderPath] } },
  });
  assert.deepEqual(explicitProviderRequest.provider_plugin_paths, [explicitProviderPath]);

  const configuredLibraryPath = path.join(defaultsRoot, 'configured-library');
  const configuredRuntimeOverlays = [{
    kind: 'bundled-library',
    library: 'php-ai-client',
    source: configuredLibraryPath,
    target: '/wordpress/wp-includes/php-ai-client',
  }];
  const configuredGenericStackRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'configured-generic-stack-task-123',
    executor: {
      backend: 'codebox',
      config: { provider: 'codex' },
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: {
      wp_codebox_provider_plugin_paths: [explicitProviderPath],
      wp_codebox_runtime_overlay_profiles: ['configured-profile'],
      wp_codebox_runtime_overlays: configuredRuntimeOverlays,
      wp_codebox_secret_env: ['CONFIGURED_SECRET'],
    },
  });
  assert.deepEqual(configuredGenericStackRequest.provider_plugin_paths, [explicitProviderPath]);
  assert.deepEqual(configuredGenericStackRequest.runtime_overlay_profiles, ['configured-profile']);
  assert.deepEqual(configuredGenericStackRequest.runtime_overlays, configuredRuntimeOverlays);
  assert.deepEqual(configuredGenericStackRequest.secret_env, ['CONFIGURED_SECRET']);

  const explicitPhpAiClientPath = path.join(defaultsRoot, 'explicit-php-ai-client');
  fs.mkdirSync(explicitPhpAiClientPath, { recursive: true });
  const explicitPhpAiClientRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'explicit-php-ai-client-runtime-stack-task-123',
    executor: {
      backend: 'codebox',
      config: { provider: 'codex' },
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  }, {
    settings: { wp_codebox_php_ai_client_path: explicitPhpAiClientPath },
  });
  assert.equal(fs.realpathSync(explicitPhpAiClientRequest.runtime_overlays[0].source), fs.realpathSync(explicitPhpAiClientPath));
  assert.equal(explicitPhpAiClientRequest.runtime_overlays[0].strategy, 'wordpress-scoped-bundle');

  const originalCwd = process.cwd();
  const labOffloadCwd = path.join(defaultsRoot, '_lab_workspaces', 'example-repo-pilot-homeboy-agent-loop');
  fs.mkdirSync(labOffloadCwd, { recursive: true });
  try {
    process.chdir(labOffloadCwd);
    const labNoTargetDefaultedRequest = codeboxTaskRequestFromAgentTaskRequest({
      ...request,
      task_id: 'lab-no-target-default-runtime-stack-task-123',
      executor: {
        backend: 'codebox',
        config: exampleAgentCiCodeboxExecutorConfig({ provider: 'codex' }),
      },
      inputs: {},
    }, {
      settings: {},
    });
    assert.deepEqual(labNoTargetDefaultedRequest.runtime_overlays, []);
    assert(!JSON.stringify(labNoTargetDefaultedRequest).includes('php-ai-client@custom-provider-auth'));
  } finally {
    process.chdir(originalCwd);
  }

  const explicitOverrideRequest = codeboxTaskRequestFromAgentTaskRequest({
    ...request,
    task_id: 'explicit-runtime-stack-task-123',
    executor: {
      backend: 'codebox',
      config: exampleAgentCiCodeboxExecutorConfig({
        provider: 'codex',
        runtime_component_paths: {
          agents_api: staleStandaloneAgentsApiPath,
          agent_runtime: '/explicit/example-runtime',
          agent_runtime_tools: '/explicit/example-runtime-tools',
        },
        provider_plugin_paths: ['/explicit/provider'],
        runtime_overlay_profiles: ['explicit-profile'],
        runtime_overlays: [{ kind: 'bundled-library', library: 'php-ai-client', source: '/explicit/php-ai-client' }],
        secret_env: ['EXPLICIT_SECRET'],
        mounts: [{ source: '/explicit/worktree', target: '/workspace', mode: 'readonly' }],
        workspaces: [{ target: '/explicit-workspace', mode: 'readonly' }],
      }),
    },
    inputs: {
      target: { root: workspaceRoot },
    },
  });
  assert.equal(explicitOverrideRequest.runtime_component_paths.agents_api, staleStandaloneAgentsApiPath);
  assert.equal(explicitOverrideRequest.runtime_component_paths.agent_runtime, '/explicit/example-runtime');
  assert.equal(explicitOverrideRequest.runtime_component_paths.agent_runtime_tools, '/explicit/example-runtime-tools');
  assert.deepEqual(explicitOverrideRequest.provider_plugin_paths, ['/explicit/provider']);
  assert.deepEqual(explicitOverrideRequest.runtime_overlay_profiles, ['explicit-profile']);
  assert.deepEqual(explicitOverrideRequest.runtime_overlays, [{ kind: 'bundled-library', library: 'php-ai-client', source: '/explicit/php-ai-client' }]);
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
    config: exampleAgentCiCodeboxExecutorConfig({
      execution_kind: 'agent_bundle',
      provider: 'openai',
      provider_plugin_paths: ['/components/ai-provider-for-openai'],
      runtime_component_paths: {
        agents_api: '/components/agents-api',
        agent_runtime: '/components/example-runtime',
        agent_runtime_tools: '/components/example-runtime-tools',
      },
      homeboy_extensions: '/components/homeboy-extensions/wordpress',
      bundle_path: '/bundles/example-agent',
      bundle_host_path: '/home/runner/work/example-repo/example-repo/bundles/example-agent',
      agent_slug: 'example-agent',
      pipeline_slug: 'example-pipeline',
      flow_slug: 'example-manual-flow',
      target_repo: 'example-org/example-repo',
      pipeline_step_patches: [{ slug: 'generate', config: { max_turns: 4 } }],
      flow_step_patches: [{ slug: 'run-pipeline', config: { step_budget: 12 } }],
      evidence_projections: [{ operation: 'github/create-pull-request', outputs: { example_pr_url: 'data.html_url' } }],
      runtime_output_projections: { example_pr_url: 'metadata.engine_data.example_agent.pr_url' },
      transcript_artifact_name: 'example-agent-transcript',
      replay_bundle_artifact_name: 'example-agent-replay',
      runner_workspace: { handle: 'example-repo@example-loop', expose_to_agent: false },
    }),
  },
});
assert.equal(agentBundleRequest.agent_bundle.bundle_path, '/bundles/example-agent');
assert.equal(agentBundleRequest.agent_bundle.agent_slug, 'example-agent');
assert.equal(agentBundleRequest.agent_bundle.pipeline_slug, 'example-pipeline');
assert.equal(agentBundleRequest.agent_bundle.flow_slug, 'example-manual-flow');
assert.deepEqual(agentBundleRequest.agent_bundle.pipeline_step_patches, [{ slug: 'generate', config: { max_turns: 4 } }]);
assert.deepEqual(agentBundleRequest.agent_bundle.flow_step_patches, [{ slug: 'run-pipeline', config: { step_budget: 12 } }]);
assert.deepEqual(agentBundleRequest.agent_bundle.evidence_projections, [{ operation: 'github/create-pull-request', outputs: { example_pr_url: 'data.html_url' } }]);
assert.deepEqual(agentBundleRequest.agent_bundle.runtime_output_projections, { example_pr_url: 'metadata.engine_data.example_agent.pr_url' });
assert.equal(Object.hasOwn(agentBundleRequest.agent_bundle, 'tool_recorders'), false);
assert.equal(Object.hasOwn(agentBundleRequest.agent_bundle, 'engine_data_outputs'), false);
assert.equal(agentBundleRequest.runtime_component_paths.agent_runtime, '/components/example-runtime');
assert.equal(agentBundleRequest.runtime_component_paths.agent_runtime_tools, '/components/example-runtime-tools');
assert.equal(agentBundleRequest.homeboy_extensions_path, '/components/homeboy-extensions/wordpress');
assert.deepEqual(agentBundleRequest.mounts, [{
  source: '/home/runner/work/example-repo/example-repo/bundles/example-agent',
  target: '/bundles/example-agent',
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
        source: '/custom/example-agent',
        target: '/bundles/example-agent',
        mode: 'readonly',
        metadata: { kind: 'custom' },
      }],
      bundle_path: '/bundles/example-agent',
      bundle_host_path: '/home/runner/work/example-repo/example-repo/bundles/example-agent',
    },
  },
});
assert.equal(agentBundleRequestWithExplicitMount.mounts.length, 1);
assert.equal(agentBundleRequestWithExplicitMount.mounts[0].source, '/custom/example-agent');

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

const canonicalArtifactEnvelopeOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  artifact_result: {
    schema: 'wp-codebox/artifact-result-envelope/v1',
    status: 'created',
    artifactRefs: [{ kind: 'artifact-log', path: '/tmp/canonical/log.txt' }],
    result: {
      outputs: {
        typed_artifacts: {
          review: { type: 'json', payload: { canonical: true } },
        },
      },
    },
  },
  run: {
    agentResult: {
      artifacts: { directory: '/tmp/legacy-artifacts' },
      patch: { artifact: 'files/legacy.patch' },
    },
  },
});
assert.equal(canonicalArtifactEnvelopeOutcome.outputs.typed_artifacts.review.payload.canonical, true);
assert.equal(canonicalArtifactEnvelopeOutcome.artifacts.some((artifact) => artifact.path === '/tmp/canonical/log.txt'), true);
assert.equal(canonicalArtifactEnvelopeOutcome.artifacts.some((artifact) => artifact.path === '/tmp/legacy-artifacts/files/legacy.patch'), false);

const failedUpstreamRunnerOutcome = agentTaskOutcomeFromCodeboxResult(request, {
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

const failedStatusBeatsSuccessfulNormalizerOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'failed',
  summary: 'WP Codebox reported failure without success false.',
}, {
  normalizeAgentTaskRunResult: () => ({
    schema: 'wp-codebox/agent-task-run-result/v1',
    status: 'succeeded',
    success: true,
    summary: 'Misleading normalizer success.',
    artifacts: [],
    diagnostics: [],
    metadata: {},
    refs: {},
  }),
  exitStatus: 0,
});
assert.equal(failedStatusBeatsSuccessfulNormalizerOutcome.status, 'failed');
assert.equal(failedStatusBeatsSuccessfulNormalizerOutcome.failure_classification, 'execution_failed');

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
    issue_url: 'https://github.com/example-org/example-repo/issues/123',
  },
  session: { id: 'sandbox-session-1', status: 'completed' },
}, { exitStatus: 1 });
assert.equal(normalizedCompletedOutcome.status, 'failed');
assert.equal(normalizedCompletedOutcome.outputs.issue_number, 123);
assert.equal(normalizedCompletedOutcome.failure_classification, 'execution_failed');

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

const metadataAgentRuntimeFailureOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  summary: 'WP Codebox agent task succeeded.',
  metadata: {
    agent_runtime: {
      result: {
        error_message: 'Codex OAuth refresh failed.',
        error_reason: 'ai_processing_failed',
        terminal_status: 'failed - ai_processing_failed',
        reason: 'empty_data_packet_returned',
        outputs: {
          error_step_id: 'ephemeral_step_0',
        },
      },
    },
  },
});
assert.equal(metadataAgentRuntimeFailureOutcome.status, 'failed');
assert.equal(metadataAgentRuntimeFailureOutcome.summary, 'Codex OAuth refresh failed.');
assert.equal(metadataAgentRuntimeFailureOutcome.failure_classification, 'execution_failed');
assert.equal(metadataAgentRuntimeFailureOutcome.diagnostics[0].class, 'agent_runtime.failed');
assert.equal(metadataAgentRuntimeFailureOutcome.diagnostics[0].data.reason, 'ai_processing_failed');
assert.equal(metadataAgentRuntimeFailureOutcome.metadata.decision_evidence.agent_runtime_failure_reason, 'ai_processing_failed');

const metadataAgentRuntimeTerminalFailureOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  metadata: {
    agent_runtime: {
      result: {
        terminalStatus: 'failed - provider_auth_failed',
      },
    },
  },
});
assert.equal(metadataAgentRuntimeTerminalFailureOutcome.status, 'failed');
assert.equal(metadataAgentRuntimeTerminalFailureOutcome.diagnostics[0].class, 'agent_runtime.failed');
assert.equal(metadataAgentRuntimeTerminalFailureOutcome.diagnostics[0].data.reason, 'provider_auth_failed');

const workloadScenarioRuntimeFailureOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  metadata: {
    agent_runtime: {
      workload: {
        outputs: {},
        scenarios: [{
          id: 'agent-bundle',
          metadata: {
            error_step_id: 'ephemeral_step_0',
            error_reason: 'ai_processing_failed',
            terminal_status: 'failed - ai_processing_failed',
          },
        }],
      },
    },
  },
});
assert.equal(workloadScenarioRuntimeFailureOutcome.status, 'failed');
assert.equal(workloadScenarioRuntimeFailureOutcome.diagnostics[0].class, 'agent_runtime.failed');
assert.equal(workloadScenarioRuntimeFailureOutcome.diagnostics[0].data.reason, 'ai_processing_failed');
assert.equal(workloadScenarioRuntimeFailureOutcome.diagnostics[0].data.error_step_id, 'ephemeral_step_0');

const outputRuntimeFailureMetadataOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  outputs: {
    error_step_id: 'ephemeral_step_1',
    error_reason: 'provider_auth_failed',
  },
});
assert.equal(outputRuntimeFailureMetadataOutcome.status, 'failed');
assert.equal(outputRuntimeFailureMetadataOutcome.diagnostics[0].class, 'agent_runtime.failed');
assert.equal(outputRuntimeFailureMetadataOutcome.diagnostics[0].data.reason, 'provider_auth_failed');

const outputAgentRuntimeFailureWithoutTypedArtifactsOutcome = agentTaskOutcomeFromCodeboxResult(request, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  summary: 'WP Codebox agent task succeeded.',
  outputs: {
    agent_runtime: {
      success: false,
      result: {
        success: false,
        error_reason: 'ai_processing_failed',
        error_message: 'Embedded runtime failed before emitting typed artifacts.',
        terminal_status: 'failed - ai_processing_failed',
        outputs: {},
      },
    },
  },
}, {
  normalizeAgentTaskRunResult: () => ({
    schema: 'wp-codebox/agent-task-run-result/v1',
    status: 'succeeded',
    artifacts: [],
    diagnostics: [],
    metadata: {},
    refs: {},
  }),
});
assert.equal(outputAgentRuntimeFailureWithoutTypedArtifactsOutcome.status, 'failed');
assert.equal(outputAgentRuntimeFailureWithoutTypedArtifactsOutcome.summary, 'Embedded runtime failed before emitting typed artifacts.');
assert.equal(outputAgentRuntimeFailureWithoutTypedArtifactsOutcome.failure_classification, 'execution_failed');
assert.equal(outputAgentRuntimeFailureWithoutTypedArtifactsOutcome.diagnostics[0].class, 'agent_runtime.failed');
assert.equal(outputAgentRuntimeFailureWithoutTypedArtifactsOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.required_typed_artifacts_missing'), false);
assert.deepEqual(outputAgentRuntimeFailureWithoutTypedArtifactsOutcome.metadata.typed_artifacts, {});

const synthesizedArtifactRuntimeFailureOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  artifact_declarations: [
    { name: 'patch', required: true },
    { name: 'agent_result', required: true },
  ],
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  summary: 'WP Codebox agent task succeeded.',
  artifacts: [{ id: 'codebox-patch', kind: 'codebox-patch', path: '/tmp/patch.diff' }],
  outputs: {
    typed_artifacts: {
      patch: { name: 'patch', type: 'file', payload: { path: '/tmp/patch.diff' } },
      agent_result: { name: 'agent_result', type: 'json', payload: { status: 'failed' } },
    },
  },
  metadata: {
    agent_runtime: {
      workload: {
        success: false,
        status: 'failed',
        outputs: {},
      },
    },
  },
}, {
  normalizeAgentTaskRunResult: () => ({
    schema: 'wp-codebox/agent-task-run-result/v1',
    status: 'succeeded',
    artifacts: [],
    diagnostics: [],
    metadata: {},
    refs: {},
  }),
});
assert.equal(synthesizedArtifactRuntimeFailureOutcome.status, 'failed');
assert.equal(synthesizedArtifactRuntimeFailureOutcome.failure_classification, 'execution_failed');
assert.equal(synthesizedArtifactRuntimeFailureOutcome.diagnostics[0].class, 'agent_runtime.failed');

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
            example_review: {
              schema: 'homeboy/agent-task-typed-artifact/v1',
              type: 'ExampleReviewArtifact',
              artifact_schema: 'example/review-artifact/v1',
              payload: { slug: 'issue-1222-transformer-loop', review_ready: true },
              provenance: { bundle_slug: 'example-agent', task_id: 'agent-bundle-task-123' },
              file_refs: [{ path: '/tmp/wp-codebox-artifacts/example-review.json', mime: 'application/json' }],
            },
          },
        },
        scenarios: [{
          id: 'agent-bundle',
          metadata: {
            transcript_artifacts: { json: '/tmp/transcript.json', summary: '/tmp/transcript.md' },
            replay_bundle_path: '/tmp/replay-bundle',
            engine_data: { example_agent: { pr_url: 'https://github.com/example-org/example-repo/pull/123' } },
          },
        }],
      },
    },
  },
});
assert.equal(agentBundleOutcome.schema, 'homeboy/agent-task-outcome/v1');
assert.equal(agentBundleOutcome.status, 'succeeded');
assert.equal(agentBundleOutcome.outputs.example_pr_url, 'https://github.com/example-org/example-repo/pull/123');
assert.equal(agentBundleOutcome.outputs.typed_artifacts.example_review.type, 'ExampleReviewArtifact');
assert.equal(agentBundleOutcome.outputs.typed_artifacts.example_review.artifact_schema, 'example/review-artifact/v1');
assert.equal(agentBundleOutcome.outputs.typed_artifacts.example_review.payload.review_ready, true);
assert.equal(agentBundleOutcome.artifacts.some((artifact) => artifact.kind === 'typed-bundle-output' && artifact.name === 'example_review' && artifact.path === '/tmp/wp-codebox-artifacts/example-review.json'), true);
assert.equal(agentBundleOutcome.artifacts.some((artifact) => artifact.kind === 'agent-runtime-transcript' && artifact.path === '/tmp/transcript.json'), true);
assert.equal(agentBundleOutcome.artifacts.some((artifact) => artifact.kind === 'agent-runtime-replay-bundle' && artifact.path === '/tmp/replay-bundle'), true);
assert.equal(agentBundleOutcome.evidence_refs.some((ref) => ref.uri === 'https://github.com/example-org/example-repo/pull/123'), true);
assert.equal(agentBundleOutcome.evidence_refs.some((ref) => ref.uri === '/tmp/wp-codebox-artifacts/example-review.json'), true);
assert.equal(agentBundleOutcome.metadata.sandbox_policy.policy.apply, 'review');
assert.equal(agentBundleOutcome.metadata.sandbox_policy.sandbox_tool_policy.tools[0].allowed, false);
assert.equal(upstreamRunnerOutcome.artifacts[1].kind, 'codebox-session-artifacts');
assert.equal(upstreamRunnerOutcome.evidence_refs[0].uri, 'https://preview.example.test/sandbox-session-1');
assert.equal(upstreamRunnerOutcome.evidence_refs[1].uri, '/tmp/wp-codebox-artifacts');

const missingRequiredTypedArtifactOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'missing-required-typed-artifact-task-123',
  artifact_declarations: [{
    schema: 'wp-codebox/artifact-declaration/v1',
    name: 'required_report',
    type: 'RequiredReport',
    artifact_schema: 'example/required-report/v1',
    required: true,
  }],
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  metadata: {
    agent_runtime: {
      workload: {
        outputs: {},
        scenarios: [{ id: 'agent-bundle', metadata: {} }],
      },
    },
  },
});
assert.equal(missingRequiredTypedArtifactOutcome.status, 'failed');
assert.equal(missingRequiredTypedArtifactOutcome.failure_classification, 'execution_failed');
assert.equal(missingRequiredTypedArtifactOutcome.diagnostics[0].class, 'codebox.required_typed_artifacts_missing');
assert.equal(missingRequiredTypedArtifactOutcome.diagnostics[0].data.missing[0].name, 'required_report');

const inputBackfilledTypedArtifactOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'input-backfilled-typed-artifact-task-123',
  inputs: {
    required_report: { review_ready: true },
  },
  artifact_declarations: [{
    schema: 'wp-codebox/artifact-declaration/v1',
    name: 'required_report',
    type: 'RequiredReport',
    artifact_schema: 'example/required-report/v1',
    required: true,
  }],
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  metadata: {
    agent_runtime: {
      workload: {
        outputs: {},
        scenarios: [{ id: 'agent-bundle', metadata: {} }],
      },
    },
  },
});
assert.equal(inputBackfilledTypedArtifactOutcome.status, 'succeeded');
assert.equal(inputBackfilledTypedArtifactOutcome.outputs.typed_artifacts.required_report.artifact_schema, 'example/required-report/v1');
assert.equal(inputBackfilledTypedArtifactOutcome.outputs.typed_artifacts.required_report.payload.review_ready, true);
assert.equal(inputBackfilledTypedArtifactOutcome.outputs.typed_artifacts.required_report.metadata.normalized_from, 'request_input');
assert.equal(inputBackfilledTypedArtifactOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.required_typed_artifacts_missing'), false);

const missingGenericRepoLoopArtifactOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'missing-generic-repo-loop-artifact-task-123',
  artifacts: {
    outputs: {
      concept_packet: {
        type: 'ConceptPacket',
        schema: 'example/concept-packet/v1',
      },
    },
  },
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  metadata: {
    agent_runtime: {
      workload: {
        outputs: {},
        scenarios: [{ id: 'agent-bundle', metadata: {} }],
      },
    },
  },
});
assert.equal(missingGenericRepoLoopArtifactOutcome.status, 'failed');
assert.equal(missingGenericRepoLoopArtifactOutcome.failure_classification, 'execution_failed');
assert.equal(missingGenericRepoLoopArtifactOutcome.diagnostics[0].class, 'codebox.required_typed_artifacts_missing');
assert.equal(missingGenericRepoLoopArtifactOutcome.diagnostics[0].data.missing[0].name, 'concept_packet');
assert.equal(missingGenericRepoLoopArtifactOutcome.diagnostics[0].data.missing[0].artifact_schema, 'example/concept-packet/v1');

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
          example_branch: 'metadata.engine_data.example_agent.branch',
          example_pr_url: 'metadata.engine_data.example_agent.pr_url',
          example_slug: 'metadata.engine_data.example_agent.slug',
        },
      },
      workload: {
        outputs: [],
      },
    },
  },
  outputs: {
    engine_data: {
      example_agent: {
        branch: 'example/issue-451-design-direction',
        pr_url: 'https://github.com/example-org/example-repo/pull/453',
        slug: 'issue-451-design-direction',
      },
    },
  },
});
assert.equal(canonicalTopLevelAgentBundleOutcome.status, 'succeeded');
assert.equal(canonicalTopLevelAgentBundleOutcome.outputs.example_branch, 'example/issue-451-design-direction');
assert.equal(canonicalTopLevelAgentBundleOutcome.outputs.example_pr_url, 'https://github.com/example-org/example-repo/pull/453');
assert.equal(canonicalTopLevelAgentBundleOutcome.outputs.example_slug, 'issue-451-design-direction');
assert.equal(canonicalTopLevelAgentBundleOutcome.evidence_refs.some((ref) => ref.uri === 'https://github.com/example-org/example-repo/pull/453'), true);

const projectedTypedArtifactBundleOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'projected-typed-artifact-bundle-task-123',
  artifact_declarations: [{
    schema: 'wp-codebox/artifact-declaration/v1',
    name: 'example_review',
    type: 'ExampleReviewArtifact',
    artifact_schema: 'example/review-artifact/v1',
    required: true,
  }],
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  artifacts: '/tmp/wp-codebox-artifacts',
  metadata: {
    agent_runtime: {
      bundle: {
        engine_data_outputs: {
          example_review: 'outputs.typed_artifacts.example_review.payload',
        },
      },
      workload: {
        scenarios: [{
          id: 'agent-bundle',
          metadata: {
            schema: 'datamachine/agent-bundle-run/v1',
            outputs: {
              typed_artifacts: {
                example_review: {
                  schema: 'homeboy/agent-task-typed-artifact/v1',
                  type: 'ExampleReviewArtifact',
                  artifact_schema: 'example/review-artifact/v1',
                  payload: { slug: 'projected-review', review_ready: true },
                  provenance: { bundle_slug: 'example-agent' },
                  file_refs: [{ path: '/tmp/wp-codebox-artifacts/projected-review.json', mime: 'application/json' }],
                },
              },
            },
          },
        }],
      },
    },
  },
});
assert.equal(projectedTypedArtifactBundleOutcome.status, 'succeeded');
assert.equal(projectedTypedArtifactBundleOutcome.outputs.example_review.review_ready, true);
assert.equal(projectedTypedArtifactBundleOutcome.outputs.typed_artifacts.example_review.type, 'ExampleReviewArtifact');
assert.equal(projectedTypedArtifactBundleOutcome.outputs.typed_artifacts.example_review.payload.slug, 'projected-review');
assert.equal(projectedTypedArtifactBundleOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.required_typed_artifacts_missing'), false);
assert.equal(projectedTypedArtifactBundleOutcome.artifacts.some((artifact) => artifact.kind === 'typed-bundle-output' && artifact.path === '/tmp/wp-codebox-artifacts/projected-review.json'), true);

const engineDataTypedArtifactBundleOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'engine-data-typed-artifact-bundle-task-123',
  artifact_declarations: [{
    schema: 'wp-codebox/artifact-declaration/v1',
    name: 'concept_packet',
    type: 'ConceptPacket',
    artifact_schema: 'example/concept-packet/v1',
    required: true,
  }],
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  metadata: {
    agent_runtime: {
      workload: {
        scenarios: [{
          id: 'agent-bundle',
          metadata: {
            engine_data: {
              outputs: {
                typed_artifacts: {
                  concept_packet: {
                    schema: 'example/concept-packet/v1',
                    artifact: 'ConceptPacket',
                    payload: { title: 'Projected concept' },
                  },
                },
              },
            },
          },
        }],
      },
    },
  },
});
assert.equal(engineDataTypedArtifactBundleOutcome.status, 'succeeded');
assert.equal(engineDataTypedArtifactBundleOutcome.outputs.typed_artifacts.concept_packet.artifact_schema, 'example/concept-packet/v1');
assert.equal(engineDataTypedArtifactBundleOutcome.outputs.typed_artifacts.concept_packet.payload.title, 'Projected concept');
assert.equal(engineDataTypedArtifactBundleOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.required_typed_artifacts_missing'), false);

const replyTypedArtifactBundleOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'reply-typed-artifact-bundle-task-123',
  artifact_declarations: [{
    schema: 'wp-codebox/artifact-declaration/v1',
    name: 'concept_packet',
    type: 'ConceptPacket',
    artifact_schema: 'example/concept-packet/v1',
    required: true,
  }],
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  metadata: {
    agent_runtime: {
      workload: {
        id: 'runtime-task',
        success: true,
        status: 'completed',
        outputs: {},
        metadata: {
          result: {
            reply: '## Commerce Concept Packet\n\nA focused commerce concept packet.',
          },
        },
      },
    },
  },
});
assert.equal(replyTypedArtifactBundleOutcome.status, 'succeeded');
assert.equal(replyTypedArtifactBundleOutcome.outputs.typed_artifacts.concept_packet.artifact_schema, 'example/concept-packet/v1');
assert.equal(replyTypedArtifactBundleOutcome.outputs.typed_artifacts.concept_packet.artifact_id, 'concept_packet');
assert.equal(replyTypedArtifactBundleOutcome.outputs.typed_artifacts.concept_packet.kind, 'example/concept-packet/v1');
assert.match(replyTypedArtifactBundleOutcome.outputs.typed_artifacts.concept_packet.payload.content, /Commerce Concept Packet/);
assert.equal(replyTypedArtifactBundleOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.required_typed_artifacts_missing'), false);
assert.equal(replyTypedArtifactBundleOutcome.typed_artifacts.some((artifact) => artifact.name === 'concept_packet'), true);
assert.equal(replyTypedArtifactBundleOutcome.typed_artifacts.find((artifact) => artifact.name === 'concept_packet').artifact_id, 'concept_packet');
assert.equal(replyTypedArtifactBundleOutcome.typed_artifacts.find((artifact) => artifact.name === 'concept_packet').kind, 'example/concept-packet/v1');

const failedProjectedTypedArtifactBundleOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'failed-projected-typed-artifact-bundle-task-123',
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  status: 'completed',
  outputs: {
    agent_runtime: {
      success: false,
      result: {
        success: false,
        error_reason: 'empty_data_packet_returned',
        error_message: 'Runtime bundle returned an empty data packet.',
        terminal_status: 'failed - empty_data_packet_returned',
        outputs: {
          typed_artifacts: {
            failure_report: {
              type: 'FailureReport',
              artifact_schema: 'example/failure-report/v1',
              payload: { reason: 'empty_data_packet_returned' },
            },
          },
        },
      },
    },
  },
});
assert.equal(failedProjectedTypedArtifactBundleOutcome.status, 'failed');
assert.equal(failedProjectedTypedArtifactBundleOutcome.diagnostics[0].class, 'agent_runtime.failed');
assert.equal(failedProjectedTypedArtifactBundleOutcome.diagnostics[0].data.reason, 'empty_data_packet_returned');
assert.equal(failedProjectedTypedArtifactBundleOutcome.outputs.typed_artifacts.failure_report.payload.reason, 'empty_data_packet_returned');
assert.equal(failedProjectedTypedArtifactBundleOutcome.metadata.typed_artifacts.failure_report.artifact_schema, 'example/failure-report/v1');

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
          issue_number: 'metadata.engine_data.example_agent.issue_number',
          issue_url: 'metadata.engine_data.example_agent.issue_url',
        },
      },
      workload: {
        outputs: {
          issue_number: 123,
          issue_url: 'https://github.com/example-org/example-repo/issues/123',
        },
        diagnostics: [{ class: 'agent_runtime.output', message: 'Semantic outputs captured.' }],
      },
    },
  },
});
assert.equal(singleResultAgentBundleOutcome.schema, 'homeboy/agent-task-outcome/v1');
assert.equal(singleResultAgentBundleOutcome.status, 'succeeded');
assert.equal(singleResultAgentBundleOutcome.outputs.issue_number, 123);
assert.equal(singleResultAgentBundleOutcome.outputs.issue_url, 'https://github.com/example-org/example-repo/issues/123');
assert.equal(singleResultAgentBundleOutcome.evidence_refs.some((ref) => ref.uri === 'https://github.com/example-org/example-repo/issues/123'), true);

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
assert.equal(canaryRunOutcome.artifacts.some((artifact) => artifact.role === 'patch' && artifact.metadata.wp_codebox.kind === 'codebox-patch' && artifact.metadata.wp_codebox.raw.metadata.artifact === 'files/patch.diff'), true);
assert.equal(canaryRunOutcome.artifacts.some((artifact) => artifact.role === 'transcript' && artifact.metadata.wp_codebox.kind === 'codebox-transcript'), true);
assert.equal(canaryRunOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-runtime-log'), true);
assert.equal(canaryRunOutcome.artifacts.some((artifact) => artifact.kind === 'codebox-command-log'), true);
assert.equal(canaryRunOutcome.evidence_refs.some((ref) => ref.uri === '/tmp/canary/runtime/files/patch.diff'), true);
assert.equal(canaryRunOutcome.metadata.decision_evidence.selected_backend, 'codebox');
assert.equal(canaryRunOutcome.metadata.decision_evidence.run_id, 'run-canary');
assert.equal(canaryRunOutcome.metadata.decision_evidence.runtime_status, 'destroyed');
assert.equal(canaryRunOutcome.metadata.decision_evidence.cleanup_observed, 'runtime_destroyed');
assert.equal(canaryRunOutcome.metadata.decision_evidence.no_op_reason, 'no_file_changes');
assert.equal(canaryRunOutcome.metadata.decision_evidence.patch_bytes, 0);
assert.deepEqual(canaryRunOutcome.metadata.decision_evidence.runtime_gap_trackers.map((tracker) => tracker.gap), ['runtime-profile-normalizer', 'typed-artifact-dto-normalizer']);

const canaryTranscriptRequiredOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  artifact_declarations: [{
    name: 'datamachine-transcript',
    type: 'transcript',
    required: true,
  }],
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  run: {
    runId: 'run-canary-transcript-required',
    status: 'succeeded',
    agentResult: {
      changedFiles: { count: 0, paths: [], artifact: 'files/changed-files.json' },
      patch: { bytes: 0, sha256: 'empty-patch-sha', artifact: 'files/patch.diff' },
      transcript: { artifact: 'files/transcript.json', executionCount: 1 },
      artifacts: { directory: '/tmp/canary/runtime' },
      noOpReason: 'no_file_changes',
    },
  },
});
assert.equal(canaryTranscriptRequiredOutcome.status, 'no_op');
assert.equal(canaryTranscriptRequiredOutcome.outputs.typed_artifacts['datamachine-transcript'].file_refs[0].path, '/tmp/canary/runtime/files/transcript.json');
assert.equal(canaryTranscriptRequiredOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.required_typed_artifacts_missing'), false);

const labArtifactRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-lab-artifacts-'));
const labRuntimeRoot = path.join(labArtifactRoot, 'runtime-fixture-123');
fs.mkdirSync(path.join(labRuntimeRoot, 'files'), { recursive: true });
fs.writeFileSync(path.join(labRuntimeRoot, 'files', 'transcript.json'), '{"schema":"wp-codebox/agent-transcript/v1"}\n');
const labTranscriptRequiredOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  artifact_declarations: [{
    name: 'datamachine-transcript',
    type: 'transcript',
    required: true,
  }],
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  session: {
    id: 'lab-session-transcript-required',
    status: 'completed',
    artifacts: {
      bundle_id: 'lab-artifact-bundle',
      directory: labRuntimeRoot,
    },
  },
  artifacts: [{
    id: 'lab-codebox-transcript',
    kind: 'codebox-transcript',
    path: path.join(labRuntimeRoot, 'files', 'transcript.json'),
    mime: 'application/json',
  }],
  outputs: {},
});
assert.equal(labTranscriptRequiredOutcome.outputs.typed_artifacts['datamachine-transcript'].file_refs[0].path, path.join(labRuntimeRoot, 'files', 'transcript.json'));
assert.equal(labTranscriptRequiredOutcome.diagnostics.some((diagnostic) => diagnostic.class === 'codebox.required_typed_artifacts_missing'), false);

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

const codexProviderNotRegisteredOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'codex-provider-not-registered-task-123',
  executor: {
    backend: 'codebox',
    config: {
      provider: 'codex',
      provider_plugin_paths: ['/components/ai-provider-for-openai'],
    },
  },
}, {
  success: false,
  status: 'failed',
  summary: 'Requested provider "codex" is not registered in wp-ai-client after sandbox provider plugins were loaded.',
  diagnostics: [{
    class: 'wp_codebox_provider_not_registered',
    message: 'Requested provider "codex" is not registered in wp-ai-client after sandbox provider plugins were loaded.',
  }],
  task_input: {
    provider: 'codex',
    provider_plugin_paths: ['/components/ai-provider-for-openai'],
  },
});
const providerNotRegisteredDiagnostic = codexProviderNotRegisteredOutcome.diagnostics.find((diagnostic) => diagnostic.class === 'codebox.provider_not_registered');
assert(providerNotRegisteredDiagnostic);
assert.match(providerNotRegisteredDiagnostic.message, /registered codex provider/);
assert.deepEqual(providerNotRegisteredDiagnostic.data.provider_plugin_paths, ['/components/ai-provider-for-openai']);

const codexMissingProviderPluginOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'codex-missing-provider-plugin-task-123',
  executor: {
    backend: 'codebox',
    config: { provider: 'codex' },
  },
}, {
  success: false,
  status: 'failed',
  summary: 'Requested provider "codex" is not registered in wp-ai-client after sandbox provider plugins were loaded.',
  diagnostics: [{ class: 'wp_codebox_provider_not_registered' }],
  task_input: {
    provider: 'codex',
    provider_plugin_paths: [],
  },
});
const missingProviderPluginDiagnostic = codexMissingProviderPluginOutcome.diagnostics.find((diagnostic) => diagnostic.class === 'codebox.provider_not_registered');
assert(missingProviderPluginDiagnostic);
assert.equal(missingProviderPluginDiagnostic.data.missing_provider_plugin_path, true);
assert.deepEqual(missingProviderPluginDiagnostic.data.provider_plugin_paths, []);

const installedLayoutRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-agent-runtime-install-'));
try {
  const installedRuntime = path.join(installedLayoutRoot, 'agent-runtimes', 'wp-codebox');
  fs.mkdirSync(path.join(installedRuntime, 'scripts', 'agent'), { recursive: true });
  fs.mkdirSync(path.join(installedRuntime, 'lib'), { recursive: true });
  fs.mkdirSync(path.join(installedLayoutRoot, 'extensions', 'wordpress', 'lib'), { recursive: true });
  fs.copyFileSync(wpCodeboxRuntimeExecutor, path.join(installedRuntime, 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'));
  fs.writeFileSync(path.join(installedRuntime, 'lib', 'codebox-agent-task-executor.js'), `module.exports = {
  agentTaskOutcomeFromCodeboxResult() { return {}; },
  codeboxTaskRequestFromAgentTaskRequest() { return {}; },
  providerContract() { return {}; },
};\n`);
  fs.writeFileSync(path.join(installedRuntime, 'lib', 'provider-preflight-manifest.js'), `module.exports = {
  normalizeStringArray(value) { return Array.isArray(value) ? value : []; },
  providerAuthEnvSources() { return {}; },
  providerDiagnosticClass() { return 'fixture'; },
  providerLabel() { return 'fixture'; },
  providerPluginValidation() { return null; },
  providerSecretEnv() { return []; },
};\n`);
  fs.writeFileSync(path.join(installedRuntime, 'lib', 'codebox-artifact-contract.js'), `module.exports = {
  discoverCodeboxArtifactRefs() { return { artifacts: [], evidenceRefs: [], runtimeId: '', lastKnownPhase: '', lastHeartbeat: null }; },
};\n`);
  fs.writeFileSync(path.join(installedLayoutRoot, 'extensions', 'wordpress', 'lib', 'wp-codebox-core-loader.js'), `module.exports = { async loadWpCodeboxCore() { return {}; } };\n`);
  const installedLayoutResult = spawnSync(process.execPath, [path.join(installedRuntime, 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs')], {
    encoding: 'utf8',
    env: fixtureEnv(),
  });
  assert.equal(installedLayoutResult.status, 1);
  assert.match(installedLayoutResult.stderr, /AgentTaskRequest JSON is required/);
  assert.doesNotMatch(installedLayoutResult.stderr, /wp-codebox-core-loader|MODULE_NOT_FOUND/);
} finally {
  fs.rmSync(installedLayoutRoot, { recursive: true, force: true });
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-agent-task-executor-'));
try {
  const { fixture, capture } = writeFixtureTaskRunner(root);
  const missingModelResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
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

  fs.rmSync(capture, { force: true });
  const invalidOverlayResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify({
      ...request,
      task_id: 'invalid-runtime-overlay-cli-task-123',
      executor: {
        ...request.executor,
        config: {
          ...request.executor.config,
          runtime_overlays: [{ type: 'bundled-library', library: 'php-ai-client', source: '/components/php-ai-client' }],
        },
      },
    }),
  });
  assert.equal(invalidOverlayResult.status, 0, invalidOverlayResult.stderr || invalidOverlayResult.stdout);
  const invalidOverlayCapture = JSON.parse(fs.readFileSync(capture, 'utf8'));
  assert.deepEqual(invalidOverlayCapture.request.runtime_overlays, [{ type: 'bundled-library', library: 'php-ai-client', source: '/components/php-ai-client' }]);

  const result = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
    '--task-runner',
    fixture,
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
    wpCodeboxRuntimeExecutor,
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

  const failingCompletedRunner = writeCompletedJsonFailingTaskRunner(root);
  const failingCompletedResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
    '--task-runner',
    failingCompletedRunner,
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
    env: fixtureEnv({ HOMEBOY_WP_CODEBOX_CORE_MODULE: fixtureCodeboxCoreModule }),
  });
  assert.equal(failingCompletedResult.status, 1, failingCompletedResult.stderr || failingCompletedResult.stdout);
  const failingCompletedOutcome = JSON.parse(failingCompletedResult.stdout);
  assert.equal(failingCompletedOutcome.status, 'failed');
  assert.equal(failingCompletedOutcome.failure_classification, 'execution_failed');
  assert.equal(failingCompletedOutcome.outputs.issue_url, 'https://github.com/example/repo/issues/456');
  assert.equal(failingCompletedOutcome.artifacts.some((artifact) => artifact.path === '/tmp/semantic.patch'), true);

  const captured = JSON.parse(fs.readFileSync(capture, 'utf8'));
  assert.equal(captured.request.schema, 'wp-codebox/task-input/v1');
  assert.equal(captured.request.orchestrator.agent_task_id, 'task-123');
  assert.equal(captured.request.runtime_overlays[0].kind, 'bundled-library');

  const recipeCliResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
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
    wpCodeboxRuntimeExecutor,
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
  assert.equal(Object.hasOwn(capturedCodex.request.runtime_component_paths, 'agents_api'), false);
  assert.equal(capturedCodex.request.runtime_component_paths.agent_runtime, '/components/example-runtime');
  assert.equal(capturedCodex.request.runtime_component_paths.agent_runtime_tools, '/components/example-runtime-tools');
  assert.equal(Object.hasOwn(capturedCodex.request, 'agents_api_path'), false);
  assert.equal(Object.hasOwn(capturedCodex.request, 'data_machine_path'), false);
  assert.equal(Object.hasOwn(capturedCodex.request, 'data_machine_code_path'), false);
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

  const missingCodexProviderPathResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
    env: fixtureEnv({ HOME: fakeCodexHome }),
    input: JSON.stringify({
      ...codexAgentRequest,
      task_id: 'codex-missing-provider-path-cli-task-123',
      executor: {
        backend: 'codebox',
        model: 'gpt-5.5',
        config: {
          provider: 'codex',
          secret_env: codexSecretEnv,
        },
      },
    }),
  });
  assert.equal(missingCodexProviderPathResult.status, 1, missingCodexProviderPathResult.stderr || missingCodexProviderPathResult.stdout);
  const missingCodexProviderPathOutcome = JSON.parse(missingCodexProviderPathResult.stdout);
  assert.equal(missingCodexProviderPathOutcome.status, 'failed');
  assert.equal(missingCodexProviderPathOutcome.failure_classification, 'provider');
  assert.equal(missingCodexProviderPathOutcome.diagnostics[0].class, 'codebox.preflight.codex_provider_plugin_path');
  assert.deepEqual(missingCodexProviderPathOutcome.diagnostics[0].data.provider_plugin_paths, []);
  assert.match(missingCodexProviderPathOutcome.summary, /Codex-capable provider plugin checkout/);

  fs.rmSync(capture, { force: true });
  const defaultedCodexMissingProviderPathResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
    env: fixtureEnv({
      HOME: fakeCodexHome,
      HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_default_provider: 'codex' }),
    }),
    input: JSON.stringify({
      ...request,
      task_id: 'defaulted-codex-missing-provider-path-cli-task-123',
      executor: {
        backend: 'codebox',
        model: 'gpt-5.5',
        config: { secret_env: codexSecretEnv },
      },
    }),
  });
  assert.equal(defaultedCodexMissingProviderPathResult.status, 1, defaultedCodexMissingProviderPathResult.stderr || defaultedCodexMissingProviderPathResult.stdout);
  assert.equal(fs.existsSync(capture), false, 'task runner should not be invoked when defaulted Codex has no provider plugin paths');
  const defaultedCodexMissingProviderPathOutcome = JSON.parse(defaultedCodexMissingProviderPathResult.stdout);
  assert.equal(defaultedCodexMissingProviderPathOutcome.status, 'failed');
  assert.equal(defaultedCodexMissingProviderPathOutcome.failure_classification, 'provider');
  assert.equal(defaultedCodexMissingProviderPathOutcome.diagnostics[0].class, 'codebox.preflight.codex_provider_plugin_path');
  assert.equal(defaultedCodexMissingProviderPathOutcome.diagnostics[0].data.provider, 'codex');
  assert.deepEqual(defaultedCodexMissingProviderPathOutcome.diagnostics[0].data.provider_plugin_paths, []);

  const wrongCodexProviderPathResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
    env: fixtureEnv({ HOME: fakeCodexHome }),
    input: JSON.stringify({
      ...codexAgentRequest,
      task_id: 'codex-wrong-provider-path-cli-task-123',
      executor: {
        backend: 'codebox',
        model: 'gpt-5.5',
        config: {
          provider: 'codex',
          provider_plugin_paths: ['/components/ai-provider-for-opencode'],
          secret_env: codexSecretEnv,
        },
      },
    }),
  });
  assert.equal(wrongCodexProviderPathResult.status, 1, wrongCodexProviderPathResult.stderr || wrongCodexProviderPathResult.stdout);
  const wrongCodexProviderPathOutcome = JSON.parse(wrongCodexProviderPathResult.stdout);
  assert.equal(wrongCodexProviderPathOutcome.diagnostics[0].class, 'codebox.preflight.codex_provider_plugin_path');
  assert.equal(wrongCodexProviderPathOutcome.diagnostics[0].data.inspections[0].reason, 'opencode_provider_plugin');

  const releasedOpenAiProviderPath = path.join(root, 'ai-provider-for-openai');
  fs.mkdirSync(releasedOpenAiProviderPath, { recursive: true });
  fs.writeFileSync(path.join(releasedOpenAiProviderPath, 'plugin.php'), '<?php\n// Registers openai provider only.\n');
  const releasedOpenAiProviderPathResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
    env: fixtureEnv({ HOME: fakeCodexHome }),
    input: JSON.stringify({
      ...codexAgentRequest,
      task_id: 'codex-released-openai-provider-path-cli-task-123',
      executor: {
        backend: 'codebox',
        model: 'gpt-5.5',
        config: {
          provider: 'codex',
          provider_plugin_paths: [releasedOpenAiProviderPath],
          secret_env: codexSecretEnv,
        },
      },
    }),
  });
  assert.equal(releasedOpenAiProviderPathResult.status, 1, releasedOpenAiProviderPathResult.stderr || releasedOpenAiProviderPathResult.stdout);
  const releasedOpenAiProviderPathOutcome = JSON.parse(releasedOpenAiProviderPathResult.stdout);
  assert.equal(releasedOpenAiProviderPathOutcome.diagnostics[0].data.inspections[0].reason, 'no_codex_marker_found');
  assert.match(releasedOpenAiProviderPathOutcome.summary, /Released ai-provider-for-openai trunk registers openai, not codex/);

  const codexCapableProviderPath = path.join(root, 'ai-provider-for-openai-codex');
  fs.mkdirSync(codexCapableProviderPath, { recursive: true });
  fs.writeFileSync(path.join(codexCapableProviderPath, 'plugin.php'), '<?php\n// Registers the codex provider.\n');
  const codexCapableProviderPathResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
    env: fixtureEnv({ HOME: fakeCodexHome }),
    input: JSON.stringify({
      ...codexAgentRequest,
      task_id: 'codex-capable-provider-path-cli-task-123',
      executor: {
        backend: 'codebox',
        model: 'gpt-5.5',
        config: {
          provider: 'codex',
          provider_plugin_paths: [codexCapableProviderPath],
          secret_env: codexSecretEnv,
        },
      },
    }),
  });
  assert.equal(codexCapableProviderPathResult.status, 0, codexCapableProviderPathResult.stderr || codexCapableProviderPathResult.stdout);
  const capturedCodexCapableProvider = JSON.parse(fs.readFileSync(capture, 'utf8'));
  assert.deepEqual(capturedCodexCapableProvider.request.provider_plugin_paths, [codexCapableProviderPath]);

  const agentBundleRoot = fs.mkdtempSync(path.join(root, 'agent-bundle-'));
  const bundle = writeBundleFixture(agentBundleRoot);
  const { fixture: fakeWpCodebox, capture: fakeWpCodeboxCapture } = writeFakeWpCodebox(agentBundleRoot);
  const fullRunnerRuntimeEnv = {
    GENERIC_PROVIDER_CONFIG: '/runtime/provider/config.json',
    XDG_DATA_HOME: '/runtime/provider/data',
  };
  const fullRunnerRuntimeStateMounts = [{
    source: '/host/provider/state.json',
    target: '/runtime/provider/state.json',
    mode: 'readonly',
    metadata: { purpose: 'provider-state' },
  }];
  const fullRunnerRuntimeConfigMounts = [{
    source: '/host/provider/config.json',
    target: '/runtime/provider/config.json',
    mode: 'readonly',
    metadata: { purpose: 'provider-config' },
  }];
  const agentBundleCliRequest = {
    ...request,
    task_id: 'agent-bundle-cli-task-123',
    executor: {
      backend: 'codebox',
      model: 'gpt-5.5',
      config: {
        provider: 'openai',
        provider_plugin_paths: ['/components/ai-provider-for-openai'],
        runtime_component_paths: {
          agents_api: '/components/agents-api',
          agent_runtime: '/components/example-runtime',
          agent_runtime_tools: '/components/example-runtime-tools',
        },
        runtime_bundle_ability: 'example/run-agent-bundle',
        runtime_env: fullRunnerRuntimeEnv,
        runtime_state_mounts: fullRunnerRuntimeStateMounts,
        runtime_config_mounts: fullRunnerRuntimeConfigMounts,
        homeboy_extensions: path.join(__dirname, '..'),
        wp_codebox_bin: fakeWpCodebox,
        bundle_path: bundle,
        agent_slug: 'example-agent',
        pipeline_slug: 'example-pipeline',
        flow_slug: 'example-manual-flow',
        evidence_projections: [{ operation: 'github/create-pull-request', outputs: { example_pr_url: 'data.html_url' } }],
        runtime_output_projections: { example_pr_url: 'metadata.engine_data.example_agent.pr_url' },
      },
    },
  };
  const agentBundleCliResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
  ], {
    encoding: 'utf8',
    env: fixtureEnv(),
    input: JSON.stringify(agentBundleCliRequest),
  });
  assert.equal(agentBundleCliResult.status, 0, agentBundleCliResult.stderr || agentBundleCliResult.stdout);
  const agentBundleCliOutcome = JSON.parse(agentBundleCliResult.stdout);
  assert.equal(agentBundleCliOutcome.status, 'succeeded');
  assert.equal(agentBundleCliOutcome.artifacts.some((artifact) => artifact.kind === 'agent-runtime-transcript'), true);
  assert.equal(agentBundleCliOutcome.evidence_refs.some((ref) => ref.uri === 'https://github.com/example-org/example-repo/pull/123'), true);
  const capturedAgentBundleRun = JSON.parse(fs.readFileSync(fakeWpCodeboxCapture, 'utf8'));
  assert.equal(capturedAgentBundleRun.argv[0], 'agent-task-run');
  assert.equal(capturedAgentBundleRun.input.schema, 'wp-codebox/task-input/v1');
  assert.equal(Object.hasOwn(capturedAgentBundleRun.input, 'agent'), false);
  assert.equal(Object.hasOwn(capturedAgentBundleRun.input.parent_request, 'agent'), false);
  assert.deepEqual(
    Object.fromEntries(Object.entries(capturedAgentBundleRun.input.runtime_env).filter(([key]) => key !== 'HOMEBOY_CALLBACK_DATA_PATH')),
    fullRunnerRuntimeEnv
  );
  assert.match(capturedAgentBundleRun.input.runtime_env.HOMEBOY_CALLBACK_DATA_PATH, /homeboy-runtime-callback-data\.json$/);
  assert.deepEqual(capturedAgentBundleRun.input.runtime_state_mounts, fullRunnerRuntimeStateMounts);
  assert.deepEqual(capturedAgentBundleRun.input.runtime_config_mounts, fullRunnerRuntimeConfigMounts);
  assert.equal(capturedAgentBundleRun.input.agent_bundle.bundle_path, bundle);
  assert.equal(capturedAgentBundleRun.input.agent_bundle.agent_slug, 'example-agent');
  assert.equal(capturedAgentBundleRun.input.agent_bundle.pipeline_slug, 'example-pipeline');
  assert.deepEqual(capturedAgentBundleRun.input.agent_bundle.evidence_projections, [{ operation: 'github/create-pull-request', outputs: { example_pr_url: 'data.html_url' } }]);
  assert.deepEqual(capturedAgentBundleRun.input.agent_bundle.runtime_output_projections, { example_pr_url: 'metadata.engine_data.example_agent.pr_url' });
  assert.equal(Object.hasOwn(capturedAgentBundleRun.input.agent_bundle, 'tool_recorders'), false);
  assert.equal(Object.hasOwn(capturedAgentBundleRun.input.agent_bundle, 'engine_data_outputs'), false);

  const recipeWpCodeboxRoot = fs.mkdtempSync(path.join(root, 'recipe-wp-codebox-'));
  const { fixture: recipeFakeWpCodebox, capture: recipeFakeWpCodeboxCapture } = writeFakeWpCodebox(recipeWpCodeboxRoot);
  const recipeWpCodeboxResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
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
  assert.equal(Object.hasOwn(capturedRecipeWpCodeboxRun.input, 'agent'), false);
  assert.equal(capturedRecipeWpCodeboxRun.input.recipe.pack, 'example-codebox-recipes');
  assert.equal(capturedRecipeWpCodeboxRun.input.recipe.name, 'minimal-runtime');
  assert.equal(capturedRecipeWpCodeboxRun.input.recipe.target_ref, 'Extra-Chill/example#42');

  const failedWpCodeboxRoot = fs.mkdtempSync(path.join(root, 'failed-wp-codebox-'));
  const { fixture: failedFakeWpCodebox } = writeFakeWpCodebox(failedWpCodeboxRoot);
  const failedWpCodeboxResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
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
  const failedCommandEvidence = JSON.parse(fs.readFileSync(failedWpCodeboxOutcome.artifacts.find((artifact) => artifact.kind === 'codebox-command-evidence').path, 'utf8'));
  assert.equal(failedCommandEvidence.command, failedFakeWpCodebox);

  const settingsWpCodeboxRoot = fs.mkdtempSync(path.join(root, 'settings-wp-codebox-'));
  const { fixture: settingsFakeWpCodebox } = writeFakeWpCodebox(settingsWpCodeboxRoot);
  const settingsWpCodeboxResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
  ], {
    encoding: 'utf8',
    env: fixtureEnv({
      FIXTURE_WP_CODEBOX_AGENT_TASK_FAILURE: '1',
      HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: settingsFakeWpCodebox }),
    }),
    input: JSON.stringify({
      ...request,
      task_id: 'settings-wp-codebox-task-123',
      executor: {
        backend: 'codebox',
        config: {
          homeboy_extensions: path.join(__dirname, '..'),
        },
      },
    }),
  });
  assert.equal(settingsWpCodeboxResult.status, 1, settingsWpCodeboxResult.stderr || settingsWpCodeboxResult.stdout);
  const settingsWpCodeboxOutcome = JSON.parse(settingsWpCodeboxResult.stdout);
  const settingsCommandEvidence = JSON.parse(fs.readFileSync(settingsWpCodeboxOutcome.artifacts.find((artifact) => artifact.kind === 'codebox-command-evidence').path, 'utf8'));
  assert.equal(settingsCommandEvidence.command, settingsFakeWpCodebox);

  const missingConfiguredBinary = path.join(root, 'missing-wp-codebox.cjs');
  const missingConfiguredBinaryResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
  ], {
    encoding: 'utf8',
    env: fixtureEnv({ HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: missingConfiguredBinary }) }),
    input: JSON.stringify({
      ...request,
      task_id: 'missing-configured-wp-codebox-task-123',
      executor: {
        backend: 'codebox',
        config: {
          homeboy_extensions: path.join(__dirname, '..'),
        },
      },
    }),
  });
  assert.equal(missingConfiguredBinaryResult.status, 1, missingConfiguredBinaryResult.stderr || missingConfiguredBinaryResult.stdout);
  const missingConfiguredBinaryOutcome = JSON.parse(missingConfiguredBinaryResult.stdout);
  assert.equal(missingConfiguredBinaryOutcome.status, 'failed');
  assert.equal(missingConfiguredBinaryOutcome.diagnostics[0].class, 'wp-codebox.config.invalid_binary');
  assert.equal(missingConfiguredBinaryOutcome.diagnostics[0].data.wp_codebox_bin, missingConfiguredBinary);

  const emptyJsonWpCodeboxResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
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
    wpCodeboxRuntimeExecutor,
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
    wpCodeboxRuntimeExecutor,
    '--print-contract',
  ], { encoding: 'utf8' });
  assert.equal(contractResult.status, 0, contractResult.stderr || contractResult.stdout);
  const printedContract = JSON.parse(contractResult.stdout);
  assert.equal(printedContract.id, provider.id);
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
    wpCodeboxRuntimeExecutor,
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
    wpCodeboxRuntimeExecutor,
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
    wpCodeboxRuntimeExecutor,
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
        workspace_required: true,
        repo: 'a8c-intelligence',
      },
    },
  };
  fs.rmSync(missingWorkspace.capture, { force: true });
  const missingWorkspaceResult = spawnSync(process.execPath, [
    wpCodeboxRuntimeExecutor,
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
  assert.equal(missingWorkspaceOutcome.diagnostics[0].data.workspace_required, true);
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
    wpCodeboxRuntimeExecutor,
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
    wpCodeboxRuntimeExecutor,
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
