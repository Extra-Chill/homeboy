'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const sharedContracts = require('../../agent-task-contracts');
const runtimeAgentCiRunnerContract = require('../../runtime-agent-ci/lib/agent-task-runner-contract');
const runtimeAgentCiGenericPlan = require('../../runtime-agent-ci/lib/generic-agent-task-plan');
const wordpressRunnerContract = require('../lib/agent-task-runner-contract');
const wordpressGenericPlan = require('../lib/generic-agent-task-plan');

assert.equal(wordpressRunnerContract.agentTaskRunnerSpec, sharedContracts.agentTaskRunnerSpec);
assert.equal(runtimeAgentCiRunnerContract.agentTaskRunnerSpec, sharedContracts.agentTaskRunnerSpec);
assert.equal(wordpressGenericPlan.genericAgentTaskRequest, sharedContracts.genericAgentTaskRequest);
assert.equal(runtimeAgentCiGenericPlan.genericAgentTaskRequest, sharedContracts.genericAgentTaskRequest);

assert.deepEqual(
	wordpressRunnerContract.agentTaskRunnerSpec({
		backend: 'codebox',
		config: { runtime_task: {} },
		taskTimeoutSeconds: 30,
	}),
	runtimeAgentCiRunnerContract.agentTaskRunnerSpec({
		backend: 'codebox',
		config: { runtime_task: {} },
		taskTimeoutSeconds: 30,
	}),
);
assert.deepEqual(
	wordpressGenericPlan.genericAgentTaskRequest({
		taskId: 'installable-task',
		backend: 'codebox',
		config: { runtime_task: {} },
	}),
	runtimeAgentCiGenericPlan.genericAgentTaskRequest({
		taskId: 'installable-task',
		backend: 'codebox',
		config: { runtime_task: {} },
	}),
);

assert.equal(
	sharedContracts.normalizeRuntimeExecutionDescriptor({ kind: 'ability' }, { runtime_task_ability: 'example/run-task' }).ability,
	'example/run-task',
);

process.stdout.write('Generic agent contract install boundary passed\n');
