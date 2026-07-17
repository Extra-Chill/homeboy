#!/usr/bin/env node
'use strict';

if (process.argv.includes('--provider-contract')) {
	const { agent_task_executors } = require('../../opencode.json');
	process.stdout.write(`${JSON.stringify(agent_task_executors[0], null, 2)}\n`);
	process.exit(0);
}

const {
	runCliAgentTaskExecutorBin,
} = require('../../../lib/cli-agent-task-executor-bin');
const {
	executeOpenCodeAgentTask,
	outcome,
	providerContract,
} = require('../../lib/opencode-agent-task-executor');

runCliAgentTaskExecutorBin({
	execute: executeOpenCodeAgentTask,
	outcome,
	providerContract,
});
