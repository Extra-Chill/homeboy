'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  codeboxTaskRequestFromAgentTaskRequest,
  providerContract,
} = require('../lib/codebox-agent-task-executor');

const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wordpress-agent-boundary-'));

const provider = providerContract();
assert.equal(provider.id, 'wordpress.codebox-agent-task-executor');
assert.equal(provider.label, 'WP Codebox agent task executor');
assert.equal(provider.backend, 'codebox');
assert.equal(provider.integration_contract, 'homeboy-wordpress-agent-task/v1');

const manifest = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'wordpress.json'), 'utf8'));
assert.equal(manifest.agent_task_executors, undefined);
const runtime = manifest.agent_runtimes.find((candidate) => candidate.id === 'wp-codebox');
assert(runtime, 'WordPress manifest declares the WP Codebox agent runtime');
assert.equal(runtime.agent_task_executors.length, 1);
assert.deepEqual(runtime.agent_task_executors[0], {
  ...provider,
  command: 'node {{extension_path}}/../agent-runtimes/wp-codebox/scripts/agent/homeboy-codebox-agent-task-executor.cjs',
});
assert.equal(provider.capabilities.includes('tool:wpsg_materialize_packet'), false);
assert.equal(provider.capabilities.includes('ability:wpsg_materialize_packet'), false);

const taskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'generic-wordpress-task-1',
  executor: {
    backend: 'wordpress',
    config: { provider: 'openai' },
  },
  instructions: 'Run a generic WordPress ability with declared tools.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
  tools: [
    'wordpress.read-post',
    { id: 'wordpress.write-post' },
  ],
  abilities: [
    { name: 'wordpress/site-health' },
  ],
  inputs: {
    ability_request: {
      name: 'wordpress/site-health',
      input: { include_debug: false },
    },
  },
});

assert.equal(taskInput.schema, 'wp-codebox/task-input/v1');
assert.equal(taskInput.parent_request.executor.backend, 'wordpress');
assert.deepEqual(taskInput.runtime_task, {
  ability: 'wordpress/site-health',
  input: { include_debug: false },
});
assert.deepEqual(
  taskInput.allowed_tools.filter((tool) => tool.startsWith('wordpress.')),
  ['wordpress.read-post', 'wordpress.write-post']
);
assert.equal(taskInput.allowed_tools.includes('wordpress/site-health'), true);
assert.equal(taskInput.allowed_tools.includes('workspace_apply_patch'), true);
assert.equal(taskInput.sandbox_tool_policy.schema, 'wp-codebox/sandbox-tool-policy/v1');
assert.equal(
  taskInput.sandbox_tool_policy.tools.some((tool) => tool.id === 'wordpress.read-post'),
  true
);

const repoLoopBundleTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'repo-loop-agent-bundle-task-1',
  executor: { backend: 'wordpress', config: { provider: 'openai' } },
  instructions: 'Run a repo-loop bundle workflow.',
  artifacts: {
    outputs: {
      concept_packet: {
        type: 'ConceptPacket',
        schema: 'static-site-generator/concept-packet/v1',
        required: true,
      },
    },
  },
  inputs: {
    ability_request: { name: 'datamachine/run-agent-bundle' },
    client_context: {
      inputs: {
        source: 'bundles/store-idea-agent',
        flow: 'store-idea-artifact-flow',
        wait_for_completion: true,
      },
    },
  },
});

assert.equal(repoLoopBundleTaskInput.runtime_task.ability, 'datamachine/run-agent-bundle');
assert.deepEqual(repoLoopBundleTaskInput.runtime_task.input, {
  source: 'bundles/store-idea-agent',
  flow: 'store-idea-artifact-flow',
  wait_for_completion: true,
});
assert.deepEqual(repoLoopBundleTaskInput.artifact_declarations, [{
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'concept_packet',
  type: 'ConceptPacket',
  artifact_schema: 'static-site-generator/concept-packet/v1',
  required: true,
}]);

const genericRepoLoopTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'repo-loop-generic-ability-task-1',
  executor: { backend: 'wordpress', config: { provider: 'openai' } },
  instructions: 'Run a generic declared ability with workflow inputs.',
  client_context: {
    inputs: {
      packet: 'artifact-42',
      dry_run: true,
    },
  },
  inputs: {
    ability_request: {
      name: 'example/materialize-artifact',
      input: { dry_run: false },
    },
    context: {
      inputs: { secret: 'not-forwarded-from-ambient-context' },
    },
  },
});

assert.equal(genericRepoLoopTaskInput.runtime_task.ability, 'example/materialize-artifact');
assert.deepEqual(genericRepoLoopTaskInput.runtime_task.input, {
  packet: 'artifact-42',
  dry_run: false,
});

const repoLoopTypedOutputsTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'repo-loop-typed-outputs-task-1',
  executor: { backend: 'wordpress', config: { provider: 'openai' } },
  instructions: 'Run a repo-loop step that declares typed outputs generically.',
  outputs: {
    typed_artifacts: [{
      name: 'concept_packet',
      type: 'ConceptPacket',
      artifact_schema: 'static-site-generator/concept-packet/v1',
    }],
  },
  inputs: {
    ability_request: { name: 'datamachine/run-agent-bundle' },
  },
});

assert.deepEqual(repoLoopTypedOutputsTaskInput.artifact_declarations, [{
  schema: 'wp-codebox/artifact-declaration/v1',
  name: 'concept_packet',
  type: 'ConceptPacket',
  artifact_schema: 'static-site-generator/concept-packet/v1',
  required: true,
}]);

const legacyTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'legacy-codebox-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Already queued legacy request.',
  inputs: {},
});

assert.equal(legacyTaskInput.schema, 'wp-codebox/task-input/v1');

console.log('Codebox agent-task executor boundary contract passed');
