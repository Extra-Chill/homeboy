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

function writeFixtureTaskRunner(root) {
  const fixture = path.join(root, 'fixture-task-runner.cjs');
  const capture = path.join(root, 'capture.json');
  fs.writeFileSync(fixture, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const request = JSON.parse(fs.readFileSync(0, 'utf8'));
fs.writeFileSync(${JSON.stringify(capture)}, JSON.stringify({ argv: process.argv.slice(2), request }, null, 2));
process.stdout.write(JSON.stringify({
  success: true,
  summary: 'Sandbox completed.',
  artifacts: [{ id: 'artifact-1', kind: 'screenshot', path: '/artifacts/screenshot.png' }],
  evidence_refs: [{ kind: 'preview', uri: 'https://example.test/preview', label: 'Preview' }],
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
assert.equal(provider.upstream_dependency, 'https://github.com/chubes4/wp-codebox/issues/480');
assert.equal(provider.capabilities.includes('browser_runtime'), true);

const codeboxRequest = codeboxTaskRequestFromAgentTaskRequest(request);
assert.equal(codeboxRequest.schema, 'homeboy/wp-codebox-task-request/v1');
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
assert.equal(codeboxRequest.task.prompt, request.instructions);
assert.equal(codeboxRequest.task.expected_artifacts[0], 'screenshot');
assert.equal(codeboxRequest.orchestrator.agent_task_id, 'task-123');
assert.equal(codeboxRequest.audit_findings[0].id, 'finding-1');

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
      data_machine: '/components/data-machine',
      data_machine_code: '/components/data-machine-code',
      homeboy: '/components/homeboy',
      homeboy_extensions: '/components/homeboy-extensions',
      wp_codebox_bin: '/bin/wp-codebox',
      max_turns: 8,
    },
  },
};
const codexRequest = codeboxTaskRequestFromAgentTaskRequest(codexAgentRequest);
assert.equal(codexRequest.provider, 'codex');
assert.equal(codexRequest.model, 'gpt-5.5');
assert.deepEqual(codexRequest.provider_plugin_paths, ['/components/ai-provider-for-openai']);
assert.equal(codexRequest.agents_api, '/components/agents-api');
assert.equal(codexRequest.data_machine, '/components/data-machine');
assert.equal(codexRequest.data_machine_code, '/components/data-machine-code');
assert.equal(codexRequest.homeboy, '/components/homeboy');
assert.equal(codexRequest.homeboy_extensions, '/components/homeboy-extensions');
assert.equal(codexRequest.wp_codebox_bin, '/bin/wp-codebox');
assert.deepEqual(codexRequest.secret_env, [
  'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
  'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
  'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
]);
assert(!JSON.stringify(codexRequest).includes('wp-ai-gateway'));
assert.equal(codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  limits: { task_timeout_seconds: 7 },
}).task_timeout_seconds, 7);

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
  const result = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    fixture,
    '--agents-api',
    '/components/agents-api',
    '--data-machine',
    '/components/data-machine',
    '--data-machine-code',
    '/components/data-machine-code',
  ], {
    encoding: 'utf8',
    input: JSON.stringify(request),
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const cliOutcome = JSON.parse(result.stdout);
  assert.equal(cliOutcome.status, 'succeeded');
  assert.equal(cliOutcome.artifacts[0].kind, 'screenshot');
  assert.equal(cliOutcome.evidence_refs[0].uri, 'https://example.test/preview');

  const captured = JSON.parse(fs.readFileSync(capture, 'utf8'));
  assert.equal(captured.request.schema, 'homeboy/wp-codebox-task-request/v1');
  assert.equal(captured.request.orchestrator.agent_task_id, 'task-123');
  assert.equal(captured.request.runtime_overlays[0].type, 'bundled-library');

  const codexResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--task-runner',
    fixture,
  ], {
    encoding: 'utf8',
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
  assert.equal(capturedCodex.request.agents_api, '/components/agents-api');
  assert.equal(capturedCodex.request.data_machine, '/components/data-machine');
  assert.equal(capturedCodex.request.data_machine_code, '/components/data-machine-code');
  assert.equal(capturedCodex.request.homeboy, '/components/homeboy');
  assert.equal(capturedCodex.request.homeboy_extensions, '/components/homeboy-extensions');
  assert.deepEqual(capturedCodex.request.secret_env, [
    'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
    'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
    'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
    'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
  ]);
  assert(!JSON.stringify(capturedCodex).includes('wp-ai-gateway'));

  const contractResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
    '--print-contract',
  ], { encoding: 'utf8' });
  assert.equal(contractResult.status, 0, contractResult.stderr || contractResult.stdout);
  assert.equal(JSON.parse(contractResult.stdout).id, 'wordpress.codebox-agent-task-executor');

  const artifactRoot = path.join(root, 'timeout-artifacts');
  const bundleRoot = path.join(artifactRoot, 'artifact-task-timeout');
  const filesRoot = path.join(bundleRoot, 'files');
  fs.mkdirSync(filesRoot, { recursive: true });
  fs.writeFileSync(path.join(bundleRoot, 'manifest.json'), JSON.stringify({
    schema: 'wp-codebox/artifact-manifest/v1',
    phase: 'agent.inspecting-runtime',
  }));
  fs.writeFileSync(path.join(filesRoot, 'runtime-reference-manifest.json'), JSON.stringify({
    schema: 'wp-codebox/runtime-reference-manifest/v1',
    runtime: { id: 'runtime-task-timeout' },
  }));
  fs.writeFileSync(path.join(filesRoot, 'command.log'), 'ran wp-codebox.agent-sandbox-run\n');
  fs.writeFileSync(path.join(filesRoot, 'agent-transcript.jsonl'), '{"role":"assistant","content":"partial transcript"}\n');
  fs.writeFileSync(path.join(filesRoot, 'heartbeat.json'), JSON.stringify({
    phase: 'agent.inspecting-runtime',
    heartbeat: { at: '2026-06-01T00:00:00.000Z', turn: 3 },
  }));
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
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}

console.log('Codebox agent task executor smoke passed');
