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

function writeFakeWpCodebox(root) {
  const fixture = path.join(root, 'fake-wp-codebox.cjs');
  const capture = path.join(root, 'fake-wp-codebox-capture.json');
  fs.writeFileSync(fixture, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const recipePath = process.argv[process.argv.indexOf('--recipe') + 1];
const artifacts = process.argv[process.argv.indexOf('--artifacts') + 1];
const recipe = JSON.parse(fs.readFileSync(recipePath, 'utf8'));
const codeFileArg = recipe.workflow.steps[0].args.find((arg) => arg.startsWith('code-file=')) || '';
const codeFilePath = codeFileArg.slice('code-file='.length);
if (codeFilePath) {
  fs.readFileSync(codeFilePath, 'utf8');
}
const datamachineConfig = JSON.parse(process.env.HOMEBOY_DATAMACHINE_AGENT_CONFIG || '{}');
const datamachineWorkload = {
  metrics: { config_present: 1 },
  metadata: {
    transcript_artifacts: { json: artifacts + '/transcript.json' },
    replay_bundle_path: artifacts + '/replay-bundle',
    engine_data: { static_site_agent: { pr_url: 'https://github.com/chubes4/wp-site-generator/pull/123' } }
  }
};
fs.writeFileSync(${JSON.stringify(capture)}, JSON.stringify({ argv: process.argv.slice(2), recipe, datamachineConfig }, null, 2));
process.stdout.write(JSON.stringify({
  success: false,
  executions: [{
    recipeCommand: 'wp-codebox.agent-sandbox-run',
    stdout: JSON.stringify({ status: 'completed', output: JSON.stringify(datamachineWorkload) })
  }],
  artifacts: { id: 'fake-artifact-bundle', directory: artifacts }
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
assert.equal(provider.upstream_dependency, 'https://github.com/Automattic/wp-codebox/issues/480');
assert.equal(provider.capabilities.includes('browser_runtime'), true);
assert.equal(provider.capabilities.includes('workspace_tools'), true);
assert.equal(provider.capabilities.includes('patch_artifacts'), true);
assert.equal(provider.capabilities.includes('cleanup_observability'), true);
assert.equal(provider.capabilities.includes('datamachine_bundle_execution'), true);
assert.deepEqual(provider.runtime_gap_trackers, [
  'https://github.com/Automattic/wp-codebox/issues/529',
  'https://github.com/Automattic/wp-codebox/issues/530',
  'https://github.com/Automattic/wp-codebox/issues/531',
  'https://github.com/Automattic/wp-codebox/issues/532',
]);

const codeboxRequest = codeboxTaskRequestFromAgentTaskRequest(request);
assert.equal(codeboxRequest.schema, 'homeboy/wp-codebox-task-request/v1');
assert.equal(codeboxRequest.sandbox_session_id, 'task-123');
assert.equal(codeboxRequest.execution_kind, 'sandbox');
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
      wp_cli_bin: '/bin/wp',
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
assert.equal(codexRequest.agents_api, '/components/agents-api');
assert.equal(codexRequest.data_machine, '/components/data-machine');
assert.equal(codexRequest.data_machine_code, '/components/data-machine-code');
assert.equal(codexRequest.homeboy, '/components/homeboy');
assert.equal(codexRequest.homeboy_extensions, '/components/homeboy-extensions');
assert.equal(codexRequest.wp_cli_bin, '/bin/wp');
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

const datamachineBundleRequest = codeboxTaskRequestFromAgentTaskRequest({
  ...request,
  task_id: 'datamachine-task-123',
  executor: {
    backend: 'codebox',
    model: 'gpt-5.5',
    config: {
      execution_kind: 'datamachine_bundle',
      provider: 'openai',
      provider_plugin_paths: ['/components/ai-provider-for-openai'],
      agents_api: '/components/agents-api',
      data_machine: '/components/data-machine',
      data_machine_code: '/components/data-machine-code',
      homeboy_extensions: '/components/homeboy-extensions/wordpress',
      bundle_path: '/bundles/static-site-agent',
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
assert.equal(datamachineBundleRequest.execution_kind, 'datamachine_bundle');
assert.equal(datamachineBundleRequest.datamachine_bundle.bundle_path, '/bundles/static-site-agent');
assert.equal(datamachineBundleRequest.datamachine_bundle.agent_slug, 'static-site-agent');
assert.equal(datamachineBundleRequest.datamachine_bundle.pipeline_slug, 'static-site-pipeline');
assert.equal(datamachineBundleRequest.datamachine_bundle.flow_slug, 'static-site-manual-flow');
assert.deepEqual(datamachineBundleRequest.datamachine_bundle.pipeline_step_patches, [{ slug: 'generate', config: { max_turns: 4 } }]);
assert.deepEqual(datamachineBundleRequest.datamachine_bundle.flow_step_patches, [{ slug: 'run-pipeline', config: { step_budget: 12 } }]);
assert.deepEqual(datamachineBundleRequest.datamachine_bundle.tool_recorders, [{ tool: 'github/create-pull-request', engine_data_path: 'static_site_agent.pr_url' }]);
assert.deepEqual(datamachineBundleRequest.datamachine_bundle.engine_data_outputs, { static_site_pr_url: 'metadata.engine_data.static_site_agent.pr_url' });
assert.equal(datamachineBundleRequest.homeboy_extensions, '/components/homeboy-extensions/wordpress');

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

const datamachineOutcome = agentTaskOutcomeFromCodeboxResult({
  ...request,
  task_id: 'datamachine-task-123',
  executor: { backend: 'codebox' },
}, {
  success: true,
  schema: 'wp-codebox/agent-task-run/v1',
  artifacts: '/tmp/wp-codebox-artifacts',
  metadata: {
    datamachine: {
      bundle: datamachineBundleRequest.datamachine_bundle,
      workload: {
        scenarios: [{
          id: 'datamachine-agent',
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
assert.equal(datamachineOutcome.schema, 'homeboy/agent-task-outcome/v1');
assert.equal(datamachineOutcome.status, 'succeeded');
assert.equal(datamachineOutcome.artifacts.some((artifact) => artifact.kind === 'datamachine-transcript' && artifact.path === '/tmp/transcript.json'), true);
assert.equal(datamachineOutcome.artifacts.some((artifact) => artifact.kind === 'datamachine-replay-bundle' && artifact.path === '/tmp/replay-bundle'), true);
assert.equal(datamachineOutcome.evidence_refs.some((ref) => ref.uri === 'https://github.com/chubes4/wp-site-generator/pull/123'), true);
assert.equal(upstreamRunnerOutcome.artifacts[1].kind, 'codebox-session-artifacts');
assert.equal(upstreamRunnerOutcome.evidence_refs[0].uri, 'https://preview.example.test/sandbox-session-1');
assert.equal(upstreamRunnerOutcome.evidence_refs[1].uri, '/tmp/wp-codebox-artifacts');

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
assert.equal(canaryRunOutcome.metadata.decision_evidence.runtime_gap_trackers.includes('https://github.com/Automattic/wp-codebox/issues/529'), true);

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

  const datamachineRoot = fs.mkdtempSync(path.join(root, 'datamachine-bundle-'));
  const bundle = writeBundleFixture(datamachineRoot);
  const { fixture: fakeWpCodebox, capture: fakeWpCodeboxCapture } = writeFakeWpCodebox(datamachineRoot);
  const datamachineCliRequest = {
    ...request,
    task_id: 'datamachine-cli-task-123',
    executor: {
      backend: 'codebox',
      model: 'gpt-5.5',
      config: {
        execution_kind: 'datamachine_bundle',
        provider: 'openai',
        provider_plugin_paths: ['/components/ai-provider-for-openai'],
        agents_api: '/components/agents-api',
        data_machine: '/components/data-machine',
        data_machine_code: '/components/data-machine-code',
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
  const datamachineCliResult = spawnSync(process.execPath, [
    path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs'),
  ], {
    encoding: 'utf8',
    input: JSON.stringify(datamachineCliRequest),
  });
  assert.equal(datamachineCliResult.status, 0, datamachineCliResult.stderr || datamachineCliResult.stdout);
  const datamachineCliOutcome = JSON.parse(datamachineCliResult.stdout);
  assert.equal(datamachineCliOutcome.status, 'succeeded');
  assert.equal(datamachineCliOutcome.artifacts.some((artifact) => artifact.kind === 'datamachine-transcript'), true);
  assert.equal(datamachineCliOutcome.evidence_refs.some((ref) => ref.uri === 'https://github.com/chubes4/wp-site-generator/pull/123'), true);
  const capturedDatamachineRun = JSON.parse(fs.readFileSync(fakeWpCodeboxCapture, 'utf8'));
  assert.equal(capturedDatamachineRun.recipe.workflow.steps[0].command, 'wp-codebox.agent-sandbox-run');
  assert.equal(capturedDatamachineRun.recipe.workflow.steps[0].args.includes(`code-file=${path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-datamachine-agent-workload-wrapper.php')}`), true);
  assert.equal(capturedDatamachineRun.recipe.inputs.mounts.some((mount) => mount.target === '/homeboy-extension'), true);
  assert.equal(capturedDatamachineRun.recipe.inputs.mounts.some((mount) => mount.target === '/wordpress/wp-content/plugins/static-site-agent'), true);
  assert.equal(capturedDatamachineRun.recipe.inputs.secretEnv.includes('HOMEBOY_DATAMACHINE_AGENT_CONFIG'), true);
  assert.equal(capturedDatamachineRun.datamachineConfig.bundle_path, '/wordpress/wp-content/plugins/static-site-agent');
  assert.equal(capturedDatamachineRun.datamachineConfig.agent_slug, 'static-site-agent');
  assert.equal(capturedDatamachineRun.datamachineConfig.pipeline_slug, 'static-site-pipeline');
  assert.deepEqual(capturedDatamachineRun.datamachineConfig.tool_recorders, [{ tool: 'github/create-pull-request', engine_data_path: 'static_site_agent.pr_url' }]);

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
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}

console.log('Codebox agent task executor smoke passed');
