import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';

const repoRoot = path.resolve(import.meta.dirname, '..');
const scriptsDir = path.join(repoRoot, '.github/scripts/datamachine-agent-ci');

function run(script, args = [], env = {}) {
  const result = spawnSync(process.execPath, [path.join(scriptsDir, script), ...args], {
    cwd: repoRoot,
    env: { ...process.env, ...env },
    encoding: 'utf8',
  });
  assert.equal(result.status, 0, `${script} failed\nSTDOUT:\n${result.stdout}\nSTDERR:\n${result.stderr}`);
  return result.stdout;
}

function readOutput(file) {
  return fs.readFileSync(file, 'utf8');
}

const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'datamachine-agent-ci-'));
const workspace = path.join(tempRoot, 'component');
const runnerTemp = path.join(tempRoot, 'runner');
fs.mkdirSync(path.join(workspace, '.ci/homeboy-extensions/wordpress/tests/fixtures/datamachine-agent-ci-driver'), { recursive: true });
fs.mkdirSync(path.join(workspace, '.ci/wp-codebox/packages/cli/dist'), { recursive: true });
fs.mkdirSync(path.join(workspace, '.ci/wp-codebox/packages/wordpress-plugin'), { recursive: true });
fs.mkdirSync(path.join(workspace, '.ci/agents-api'), { recursive: true });
fs.mkdirSync(path.join(workspace, '.ci/data-machine'), { recursive: true });
fs.mkdirSync(path.join(workspace, '.ci/data-machine-code'), { recursive: true });
fs.mkdirSync(runnerTemp, { recursive: true });
fs.writeFileSync(path.join(workspace, '.ci/wp-codebox/packages/cli/dist/index.js'), '');
fs.writeFileSync(path.join(workspace, '.ci/homeboy-extensions/wordpress/tests/fixtures/datamachine-agent-ci-driver/datamachine-agent-ci-driver.php'), '');

const githubOutput = path.join(tempRoot, 'github-output.txt');
run('auth.cjs', ['resolve-token-scope'], {
  GITHUB_OUTPUT: githubOutput,
  TARGET_REPO: 'Extra-Chill/homeboy-extensions',
  CONTEXT_REPOSITORIES: '[{"repo":"Extra-Chill/data-machine","ref":"main"}]',
});
assert.match(readOutput(githubOutput), /owner=Extra-Chill/);
assert.match(readOutput(githubOutput), /repositories=homeboy-extensions,data-machine/);

const dependencyPlan = JSON.parse(run('materialize-dependencies.cjs', ['--print-plan'], {
  VALIDATION_DEPENDENCIES: 'Extra-Chill/example@main,Extra-Chill/example@main',
  INCLUDE_AGENT_RUNTIME_DEPENDENCIES: 'true',
  AGENT_RUNTIME: 'wp-codebox',
  AGENT_RUNTIME_REF: 'main',
  AGENTS_API_REF: 'main',
  DATA_MACHINE_REF: 'main',
  DATA_MACHINE_CODE_REF: 'main',
  OPENAI_PROVIDER_REF: 'trunk',
  PROVIDER: 'openai',
  PROVIDER_PLUGIN: '{}',
}));
assert.equal(dependencyPlan.filter((entry) => entry.repo === 'Extra-Chill/example').length, 1);
assert.deepEqual(
  dependencyPlan.find((entry) => entry.repo === 'Automattic/wp-codebox'),
  { repo: 'Automattic/wp-codebox', ref: 'main', target: path.join('.ci', 'wp-codebox') }
);
assert.ok(dependencyPlan.some((entry) => entry.repo === 'WordPress/ai-provider-for-openai'));

fs.writeFileSync(githubOutput, '');
run('build-runner-config.cjs', [], {
  GITHUB_OUTPUT: githubOutput,
  GITHUB_WORKSPACE: workspace,
  RUNNER_TEMP: runnerTemp,
  GITHUB_SHA: 'abc123',
  AGENT_SLUG: 'agent',
  PIPELINE_SLUG: 'pipeline',
  FLOW_SLUG: 'flow',
  BUNDLE_PATH: 'bundle',
  TARGET_REPO: 'Extra-Chill/homeboy-extensions',
  CONTEXT_REPOSITORIES: '[{"repo":"Extra-Chill/data-machine","paths":["src"]}]',
  VERIFICATION_COMMANDS: '["npm test"]',
  DRIFT_CHECKS: '[]',
  WRITABLE_PATHS: 'src,tests',
  WORKSPACE_CONTRACT_CHECKS: '{}',
  PROVIDER: 'openai',
  MODEL: 'gpt-5.5',
  AGENT_RUNTIME: 'wp-codebox',
  AGENT_RUNTIME_REF: 'main',
  SUCCESS_REQUIRES_PR: 'true',
  SUCCESS_COMPLETION_OUTCOMES: '[]',
  MAX_TURNS: '12',
  STEP_BUDGET: '16',
  TIME_BUDGET_MS: '600000',
  EXPECTED_ARTIFACTS: '[]',
  ARTIFACT_DECLARATIONS: '[]',
  ARTIFACT_EXPORT_CONFIG: '{}',
  RULES: '{}',
  GENERAL_RULES: '[]',
  TASK_RULES: '[]',
  PROBES: '{}',
  WP_GYM_BENCHMARK_MODE: 'false',
  DRY_RUN: 'true',
  EXTRA_WP_CONFIG_DEFINES: '{}',
  RUNTIME_MOUNTS: '[]',
  RUNTIME_OVERLAYS: '[]',
  WORKLOAD_RUN_BEFORE: '[]',
  WORKLOAD_RUN_AFTER: '[]',
  DAILY_MEMORY_ENABLED: 'false',
  DISABLE_DATAMACHINE_DIRECTIVES: 'false',
  EXTRA_REQUIRED_ABILITIES: '[]',
  APP_TOKEN_REPOS: '',
  ALLOWED_REPOS: '[]',
  TOOL_RESULTS_KEY: 'github_tool_results',
  ABILITY_TOOLS: '[]',
  TOOL_RECORDERS: '[]',
  ENABLE_TERMINAL_ACTIONS: 'false',
  WP_CLI_TOOL_NAME: 'run_wp_cli',
  PIPELINE_STEP_PATCHES: '[]',
  FLOW_STEP_PATCHES: '[]',
  RUNNER_WORKSPACE_CONFIG: '{"enabled":true}',
  PROVIDER_PLUGIN: '{}',
});
const config = JSON.parse(fs.readFileSync(path.join(runnerTemp, 'datamachine-agent-config.json'), 'utf8'));
assert.equal(config.runtime_id, 'wp-codebox');
assert.equal(config.runner_workspace.checkout_path, '/workspace/homeboy-extensions');
assert.deepEqual(config.writable_paths, ['src', 'tests']);
assert.equal(config.provider_credentials.connectors_ai_openai_api_key, 'OPENAI_API_KEY');
assert.equal(config.runtime_profile, 'datamachine-agent-ci');
assert.equal(config.runtime_profiles['datamachine-agent-ci'].schema, 'wp-codebox/runtime-profile/v1');
assert.equal(config.runtime_profiles['datamachine-agent-ci'].homeboy_parent_tool_bridge.schema, 'wp-codebox/parent-tool-bridge/v1');
assert.deepEqual(config.runtime_requirements, config.runtime_profiles['datamachine-agent-ci']);
assert.equal(config.runtime_bin, path.join(workspace, '.ci/wp-codebox/packages/cli/dist/index.js'));
assert.equal(config.runtime_components.runtime, path.join(workspace, '.ci/wp-codebox/packages/wordpress-plugin'));
assert.equal(config.runtime_components.data_machine, path.join(workspace, '.ci/data-machine'));
assert.deepEqual(config.required_abilities, ['datamachine/import-agent', 'datamachine/run-flow', 'datamachine/drain-job']);

fs.writeFileSync(githubOutput, '');
run('build-runner-config.cjs', [], {
  GITHUB_OUTPUT: githubOutput,
  GITHUB_WORKSPACE: workspace,
  RUNNER_TEMP: runnerTemp,
  GITHUB_SHA: 'abc123',
  AGENT_SLUG: 'artifact-processor',
  PIPELINE_SLUG: 'artifact-pipeline',
  FLOW_SLUG: 'artifact-flow',
  BUNDLE_PATH: '',
  TARGET_REPO: 'Extra-Chill/homeboy-extensions',
  CONTEXT_REPOSITORIES: '[]',
  VERIFICATION_COMMANDS: '[]',
  DRIFT_CHECKS: '[]',
  WRITABLE_PATHS: '',
  WORKSPACE_CONTRACT_CHECKS: '{}',
  PROVIDER: 'openai',
  MODEL: 'gpt-5.5',
  AGENT_RUNTIME: 'wp-codebox',
  AGENT_RUNTIME_REF: 'main',
  RUNTIME_WORDPRESS_VERSION: '7.0',
  EXECUTION_KIND: 'runtime_task',
  ABILITY_REQUEST: '{"ability":"example/process-artifact","input":{"source_artifact":"/workspace/input/example-packet.json"}}',
  ABILITY_INPUT: '{"mode":"typed-artifact"}',
  RUNTIME_TASK: '{}',
  OUTPUT_MAPPINGS: '{"processed_packet":"result.processed_packet"}',
  COMPONENT_CONTRACTS: '[{"slug":"example-fixture-plugin","path":"/workspace/plugins/example-fixture-plugin","activate":true}]',
  SUCCESS_REQUIRES_PR: 'false',
  SUCCESS_COMPLETION_OUTCOMES: '[]',
  MAX_TURNS: '1',
  STEP_BUDGET: '1',
  TIME_BUDGET_MS: '60000',
  EXPECTED_ARTIFACTS: '["processed_packet"]',
  ARTIFACT_DECLARATIONS: '[{"name":"processed_packet","type":"example-packet","schema":"example/processed-packet/v1","required":true}]',
  ARTIFACT_EXPORT_CONFIG: '{}',
  RULES: '{}',
  GENERAL_RULES: '[]',
  TASK_RULES: '[]',
  PROBES: '{}',
  WP_GYM_BENCHMARK_MODE: 'false',
  DRY_RUN: 'true',
  EXTRA_WP_CONFIG_DEFINES: '{}',
  RUNTIME_MOUNTS: '[{"source":".ci/actions-artifacts/source-packet","target":"/workspace/input","mode":"readonly"}]',
  RUNTIME_OVERLAYS: '[]',
  WORKLOAD_RUN_BEFORE: '[]',
  WORKLOAD_RUN_AFTER: '[]',
  DAILY_MEMORY_ENABLED: 'false',
  DISABLE_DATAMACHINE_DIRECTIVES: 'false',
  EXTRA_REQUIRED_ABILITIES: '["example/process-artifact"]',
  APP_TOKEN_REPOS: '',
  ALLOWED_REPOS: '[]',
  TOOL_RESULTS_KEY: 'github_tool_results',
  ABILITY_TOOLS: '[]',
  TOOL_RECORDERS: '[]',
  ENABLE_TERMINAL_ACTIONS: 'false',
  WP_CLI_TOOL_NAME: 'run_wp_cli',
  PIPELINE_STEP_PATCHES: '[]',
  FLOW_STEP_PATCHES: '[]',
  RUNNER_WORKSPACE_CONFIG: '{}',
  PROVIDER_PLUGIN: '{}',
});
const runtimeTaskConfig = JSON.parse(fs.readFileSync(path.join(runnerTemp, 'datamachine-agent-config.json'), 'utf8'));
assert.equal(runtimeTaskConfig.execution_kind, 'runtime_task');
assert.deepEqual(runtimeTaskConfig.runtime_task, {
  ability: 'example/process-artifact',
  input: {
    source_artifact: '/workspace/input/example-packet.json',
    mode: 'typed-artifact',
  },
});
assert.deepEqual(runtimeTaskConfig.output_mappings, { processed_packet: 'result.processed_packet' });
assert.deepEqual(runtimeTaskConfig.component_contracts, [{ slug: 'example-fixture-plugin', path: '/workspace/plugins/example-fixture-plugin', activate: true }]);
assert.deepEqual(runtimeTaskConfig.runtime_requirements.component_contracts, runtimeTaskConfig.component_contracts);
assert.deepEqual(runtimeTaskConfig.runtime_requirements.extra_plugins, runtimeTaskConfig.component_contracts);
assert.equal(runtimeTaskConfig.runtime_mounts.some((mount) => mount.target === '/workspace/input' && mount.mode === 'readonly'), true);
assert.deepEqual(runtimeTaskConfig.artifact_declarations, [{ name: 'processed_packet', type: 'example-packet', schema: 'example/processed-packet/v1', required: true }]);
assert.deepEqual(runtimeTaskConfig.required_abilities, ['example/process-artifact']);

fs.writeFileSync(githubOutput, '');
run('build-runner-config.cjs', [], {
  GITHUB_OUTPUT: githubOutput,
  GITHUB_WORKSPACE: workspace,
  RUNNER_TEMP: runnerTemp,
  GITHUB_SHA: 'abc123',
  AGENT_SLUG: 'example-agent',
  PIPELINE_SLUG: 'example-pipeline',
  FLOW_SLUG: 'example-flow',
  BUNDLE_PATH: 'bundle',
  TARGET_REPO: 'Extra-Chill/homeboy-extensions',
  PROVIDER: 'openai',
  MODEL: 'gpt-5.5',
  AGENT_RUNTIME: 'wp-codebox',
  AGENT_RUNTIME_REF: 'main',
  RUNTIME_PROFILE: 'example-agent-ci',
  RUNTIME_PROFILES: JSON.stringify({
    'example-agent-ci': {
      schema: 'homeboy/runtime-profile/v1',
      id: 'example-agent-ci',
      runtime_task_ability: 'example/run-agent-bundle',
      component_path_defaults: {
        contract_slug_map: { 'example-agents': 'agent_runtime' },
        path_aliases: { agent_runtime: ['contract:agent_runtime'] },
      },
      ability_requirements: ['example/run-agent-bundle'],
    },
  }),
  RUNTIME_COMPONENTS: JSON.stringify({
    example_agents: '/workspace/components/example-agents',
  }),
  COMPONENT_CONTRACTS: '[{"slug":"example-agents","path":"/workspace/components/example-agents","activate":true}]',
  CONTEXT_REPOSITORIES: '[]',
  VERIFICATION_COMMANDS: '[]',
  DRIFT_CHECKS: '[]',
  WRITABLE_PATHS: '',
  WORKSPACE_CONTRACT_CHECKS: '{}',
  SUCCESS_REQUIRES_PR: 'false',
  SUCCESS_COMPLETION_OUTCOMES: '[]',
  MAX_TURNS: '1',
  STEP_BUDGET: '1',
  TIME_BUDGET_MS: '60000',
  EXPECTED_ARTIFACTS: '[]',
  ARTIFACT_DECLARATIONS: '[]',
  ARTIFACT_EXPORT_CONFIG: '{}',
  RULES: '{}',
  GENERAL_RULES: '[]',
  TASK_RULES: '[]',
  PROBES: '{}',
  WP_GYM_BENCHMARK_MODE: 'false',
  DRY_RUN: 'true',
  EXTRA_WP_CONFIG_DEFINES: '{}',
  RUNTIME_MOUNTS: '[]',
  RUNTIME_OVERLAYS: '[]',
  WORKLOAD_RUN_BEFORE: '[]',
  WORKLOAD_RUN_AFTER: '[]',
  DAILY_MEMORY_ENABLED: 'false',
  DISABLE_DATAMACHINE_DIRECTIVES: 'false',
  EXTRA_REQUIRED_ABILITIES: '[]',
  APP_TOKEN_REPOS: '',
  ALLOWED_REPOS: '[]',
  TOOL_RESULTS_KEY: 'github_tool_results',
  ABILITY_TOOLS: '[]',
  TOOL_RECORDERS: '[]',
  ENABLE_TERMINAL_ACTIONS: 'false',
  WP_CLI_TOOL_NAME: 'run_wp_cli',
  PIPELINE_STEP_PATCHES: '[]',
  FLOW_STEP_PATCHES: '[]',
  RUNNER_WORKSPACE_CONFIG: '{}',
  PROVIDER_PLUGIN: '{}',
});
const customProfileConfig = JSON.parse(fs.readFileSync(path.join(runnerTemp, 'datamachine-agent-config.json'), 'utf8'));
assert.equal(customProfileConfig.runtime_profile, 'example-agent-ci');
assert.equal(customProfileConfig.runtime_profiles['example-agent-ci'].runtime_task_ability, 'example/run-agent-bundle');
assert.equal(customProfileConfig.runtime_components.example_agents, '/workspace/components/example-agents');
assert.deepEqual(customProfileConfig.component_contracts, [{ slug: 'example-agents', path: '/workspace/components/example-agents', activate: true }]);
assert.equal(customProfileConfig.runtime_requirements.schema, 'wp-codebox/runtime-profile/v1');
assert.equal(customProfileConfig.runtime_requirements.homeboy_profile_schema, 'homeboy/runtime-profile/v1');
assert.deepEqual(customProfileConfig.runtime_requirements.component_contracts, customProfileConfig.component_contracts);
assert.deepEqual(customProfileConfig.runtime_requirements.extra_plugins, customProfileConfig.component_contracts);
assert.deepEqual(customProfileConfig.required_abilities, ['example/run-agent-bundle']);

const resultsFile = path.join(tempRoot, 'results.json');
fs.writeFileSync(resultsFile, JSON.stringify({ scenarios: [{ id: 'flow', metadata: { job_status: 'completed', engine_data: { value: 7 } } }] }));
fs.writeFileSync(githubOutput, '');
run('project-engine-data.cjs', [], {
  GITHUB_OUTPUT: githubOutput,
  RESULTS_FILE: resultsFile,
  FLOW_SLUG: 'flow',
  ENGINE_DATA_OUTPUTS: '{"value":"metadata.engine_data.value"}',
});
assert.match(readOutput(githubOutput), /engine_data_json=\{"value":7\}/);

fs.writeFileSync(githubOutput, '');
const transcript = path.join(tempRoot, 'transcript.json');
fs.writeFileSync(transcript, '{}');
run('artifacts-and-comments.cjs', ['resolve-transcript'], {
  GITHUB_OUTPUT: githubOutput,
  TRANSCRIPT_JSON: transcript,
  TRANSCRIPT_HOST_DIR: tempRoot,
});
assert.match(readOutput(githubOutput), new RegExp(`path=${transcript.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`));

fs.rmSync(tempRoot, { recursive: true, force: true });
