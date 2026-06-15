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
assert.equal(provider.id, 'wordpress.agent-task-executor');
assert.equal(provider.label, 'WordPress agent task executor');
assert.equal(provider.backend, 'wordpress');
assert.equal(provider.integration_contract, 'homeboy-wordpress-agent-task/v1');

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

const legacyTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'legacy-codebox-task-1',
  executor: { backend: 'codebox', config: { provider: 'openai' } },
  instructions: 'Already queued legacy request.',
  inputs: {},
});

assert.equal(legacyTaskInput.schema, 'wp-codebox/task-input/v1');

console.log('Codebox agent-task executor boundary contract passed');
