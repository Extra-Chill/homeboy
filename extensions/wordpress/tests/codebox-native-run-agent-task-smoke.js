'use strict';

/**
 * Native wp-codebox/run-agent-task self-call suppression.
 *
 * The runtime-agent-ci runner contract requires every runtime profile to
 * declare a non-empty runtime_task_ability, so the native wp-codebox profile
 * declares its own `wp-codebox/run-agent-task` ability. That ability is the
 * OUTER invocation the runner drives via the `run-agent-task` CLI command, not
 * a delegated ability that runs inside the sandbox.
 *
 * The wp-codebox sandbox agent-code only runs its native `agents/chat` default
 * handler when the inner task_input.runtime_task is empty; a non-empty
 * runtime_task is executed as a delegated ability. Propagating the runtime's own
 * `wp-codebox/run-agent-task` ability into the inner runtime_task made the
 * sandbox self-delegate run-agent-task with an input that lacks `goal`
 * ("goal is a required property of input").
 *
 * The executor must treat its own native agent-run ability as the OUTER native
 * invocation and leave the inner sandbox runtime_task empty, so the sandbox runs
 * native agents/chat with the agent + goal already present in the task input.
 * This mirrors how studio-native invokes run-agent-task with no inner
 * runtime_task. Genuine delegated abilities and the runtime-package path are
 * preserved.
 */

const assert = require('node:assert/strict');
const path = require('node:path');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(__dirname, '..', '..', 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');

const { codeboxTaskRequestFromAgentTaskRequest } = require('../../agent-runtimes/wp-codebox');
const runtimeAgentCi = require('../../runtime-agent-ci/provider-adapters');

const workspaceRoot = path.join(__dirname, 'fixtures');

// Build the executor config exactly the way the runner does from a wp-codebox
// runtime profile that declares its own native run-agent-task ability.
const nativeAgentRunExecutorConfig = runtimeAgentCi.runtimeAgentCiTaskExecutorConfig({
  runtimeProfile: 'codebox-agent-runtime',
  runtimeProfiles: {
    'codebox-agent-runtime': {
      schema: 'wp-codebox/runtime-profile/v1',
      id: 'codebox-agent-runtime',
      runtime_task_ability: 'wp-codebox/run-agent-task',
    },
  },
  provider: 'openai',
});

// The runner still resolves a runtime_task from the declared profile ability, so
// the profile contract (runtime_task_ability declared) stays satisfied.
assert.deepEqual(nativeAgentRunExecutorConfig.runtime_task, {
  ability: 'wp-codebox/run-agent-task',
  input: {},
});

const nativeAgentRunTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'codebox-native-run-agent-task-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      ...nativeAgentRunExecutorConfig,
      agent: 'technical-docs-bootstrap-agent',
      agent_bundles: [
        { source: '/workspace/bundles/technical-docs-bootstrap-agent.agent.json', on_conflict: 'skip' },
      ],
    },
  },
  instructions: 'Generate the developer docs flow natively in the sandbox.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
});

// The inner sandbox runtime_task is suppressed for the native self-call so the
// sandbox runs native agents/chat instead of self-delegating run-agent-task.
assert.equal(Object.hasOwn(nativeAgentRunTaskInput, 'runtime_task'), false);

// The goal + agent + agent_bundles reach the sandbox natively, so agents/chat
// has everything it needs without an inner runtime_task.
assert.equal(nativeAgentRunTaskInput.goal, 'Generate the developer docs flow natively in the sandbox.');
assert.equal(nativeAgentRunTaskInput.agent, 'technical-docs-bootstrap-agent');
assert.deepEqual(nativeAgentRunTaskInput.agent_bundles, [
  { source: '/workspace/bundles/technical-docs-bootstrap-agent.agent.json', on_conflict: 'skip' },
]);

// The legacy native alias is suppressed the same way.
const legacyNativeTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'codebox-native-agent-task-run-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'openai',
      agent: 'technical-docs-bootstrap-agent',
      agent_bundles: [{ source: '/workspace/bundles/x.agent.json', on_conflict: 'skip' }],
      runtime_task: { ability: 'wp-codebox/agent-task-run', input: {} },
    },
  },
  instructions: 'Run the legacy native agent-task-run alias.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
});
assert.equal(Object.hasOwn(legacyNativeTaskInput, 'runtime_task'), false);
assert.equal(legacyNativeTaskInput.goal, 'Run the legacy native agent-task-run alias.');

// A genuine delegated ability is still propagated as the inner runtime_task.
const delegatedAbilityTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'codebox-delegated-ability-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'openai',
      runtime_task: { ability: 'wordpress/site-health', input: { include_debug: true } },
    },
  },
  instructions: 'Run a delegated WordPress ability inside the sandbox.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
});
assert.deepEqual(delegatedAbilityTaskInput.runtime_task, {
  ability: 'wordpress/site-health',
  input: { include_debug: true },
});

// The runtime-package path is still propagated as the inner runtime_task.
const runtimePackageContrastTaskInput = codeboxTaskRequestFromAgentTaskRequest({
  schema: 'homeboy/agent-task-request/v1',
  task_id: 'codebox-runtime-package-contrast-1',
  executor: {
    backend: 'wp-codebox',
    config: {
      provider: 'openai',
      runtime_task: { ability: 'homeboy/run-runtime-package', input: { source: '/workspace/bundles/example' } },
    },
  },
  instructions: 'Run a runtime package inside the sandbox.',
  workspace: { root: workspaceRoot, mode: 'readwrite' },
});
assert.equal(runtimePackageContrastTaskInput.runtime_task.ability, 'wp-codebox/run-runtime-package');

console.log('Codebox native run-agent-task suppression smoke passed');
