'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const installableModules = [
	'agent-task-runner-contract.js',
	'generic-agent-task-plan.js',
];

for (const fileName of installableModules) {
	const source = fs.readFileSync(path.join(__dirname, '..', 'lib', fileName), 'utf8');
	assert.equal(
		source.includes('runtime-agent-ci'),
		false,
		`${fileName} must be installable with the WordPress extension without runtime-agent-ci`,
	);
}

const { agentTaskRunnerSpec } = require('../lib/agent-task-runner-contract');
const { genericAgentTaskRequest } = require('../lib/generic-agent-task-plan');

assert.equal(agentTaskRunnerSpec({ backend: 'codebox', config: { runtime_task: {} } }).executor.backend, 'codebox');
assert.equal(
	genericAgentTaskRequest({
		taskId: 'installable-task',
		backend: 'codebox',
		config: { runtime_task: {} },
	}).task_id,
	'installable-task',
);

process.stdout.write('Generic agent contract install boundary passed\n');
