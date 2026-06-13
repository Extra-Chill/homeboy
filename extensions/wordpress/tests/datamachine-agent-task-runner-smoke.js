'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = path.join(__dirname, '..');
const runtime = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-datamachine-agent-task-'));

try {
  const configPath = path.join(runtime, 'config.json');
  const resultsPath = path.join(runtime, 'run-results.json');
  const outcomePath = path.join(runtime, 'agent-task-outcome.json');
  const argsPath = path.join(runtime, 'wp-codebox-args.json');
  const binPath = path.join(runtime, 'wp-codebox.js');
  const bundlePath = path.join(runtime, 'bundle');
  const componentPath = path.join(runtime, 'component');
  const agentsApiPath = path.join(runtime, 'agents-api');
  const dataMachinePath = path.join(runtime, 'data-machine');
  const dataMachineCodePath = path.join(runtime, 'data-machine-code');

  fs.mkdirSync(bundlePath, { recursive: true });
  fs.mkdirSync(componentPath, { recursive: true });
  fs.mkdirSync(agentsApiPath, { recursive: true });
  fs.mkdirSync(dataMachinePath, { recursive: true });
  fs.mkdirSync(dataMachineCodePath, { recursive: true });
  fs.writeFileSync(path.join(bundlePath, 'manifest.json'), '{}\n');
  fs.writeFileSync(binPath, `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
if (args[0] !== 'agent-task-run' || !args.includes('--json')) {
  throw new Error('expected generic wp-codebox agent-task-run invocation');
}
const inputArg = args.find((arg) => arg.startsWith('--input-file='));
if (!inputArg) {
  throw new Error('missing --input-file');
}
const input = JSON.parse(fs.readFileSync(inputArg.slice('--input-file='.length), 'utf8'));
fs.writeFileSync(process.env.FAKE_WP_CODEBOX_ARGS_FILE, JSON.stringify({ args, input }, null, 2));
process.stdout.write(JSON.stringify({
  success: true,
  status: 'completed',
  metadata: {
    agent_runtime: {
      workload: {
        scenarios: [{
          id: 'agent-task-smoke',
          metrics: { config_present_mean: 1 },
          metadata: { success_status: 'no_changes' }
        }]
      }
    }
  },
  agentResult: { status: 'completed', summary: 'Smoke task completed.' }
}) + '\\n');
`, { mode: 0o755 });

  fs.writeFileSync(configPath, JSON.stringify({
    workload_id: 'agent-task-smoke',
    workload_label: 'Agent task smoke',
    component_path: componentPath,
    target_repo: 'Extra-Chill/example',
    bundle_path: '/bundles/example-agent',
    bundle_host_path: bundlePath,
    agent_slug: 'example-agent',
    pipeline_slug: 'example-pipeline',
    flow_slug: 'example-flow',
    prompt: 'Run the generic agent task smoke.',
    provider: 'example-provider',
    model: 'example-model',
    wp_codebox_bin: binPath,
    wp_codebox_components: {
      agents_api: agentsApiPath,
      data_machine: dataMachinePath,
      data_machine_code: dataMachineCodePath,
    },
  }, null, 2));

  const result = spawnSync(process.execPath, [
    path.join(root, 'scripts', 'agent', 'run-datamachine-agent-task.cjs'),
    configPath,
  ], {
    cwd: root,
    encoding: 'utf8',
    env: {
      ...process.env,
      HOMEBOY_DATAMACHINE_AGENT_RESULTS_FILE: resultsPath,
      HOMEBOY_AGENT_TASK_OUTCOME_FILE: outcomePath,
      FAKE_WP_CODEBOX_ARGS_FILE: argsPath,
    },
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  const captured = JSON.parse(fs.readFileSync(argsPath, 'utf8'));
  assert.equal(captured.args[0], 'agent-task-run');
  assert.equal(captured.input.schema, 'wp-codebox/task-input/v1');
  assert.equal(captured.input.agent_bundle.bundle_path, '/bundles/example-agent');
  assert.equal(captured.input.agent_bundle.agent_slug, 'example-agent');
  assert.equal(captured.input.agent_bundle.pipeline_slug, 'example-pipeline');
  assert.equal(captured.input.agent_bundle.flow_slug, 'example-flow');
  assert.equal(captured.input.provider, 'example-provider');
  assert.equal(captured.input.model, 'example-model');
  assert.equal(captured.input.agents_api_path, agentsApiPath);
  assert.equal(captured.input.runtime_component_paths.agent_runtime, dataMachinePath);
  assert.equal(captured.input.runtime_component_paths.agent_runtime_tools, dataMachineCodePath);

  const outcome = JSON.parse(fs.readFileSync(outcomePath, 'utf8'));
  assert.equal(outcome.schema, 'homeboy/agent-task-outcome/v1');
  assert.equal(outcome.status, 'succeeded');

  const results = JSON.parse(fs.readFileSync(resultsPath, 'utf8'));
  assert.equal(results.scenarios[0].id, 'agent-task-smoke');
  assert.equal(results.scenarios[0].metadata.success_status, 'no_changes');
} finally {
  fs.rmSync(runtime, { recursive: true, force: true });
}

console.log('Data Machine agent task runner smoke passed');
