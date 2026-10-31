#!/usr/bin/env node
'use strict';

if (process.argv.includes('--provider-contract')) {
	const { agent_task_executors } = require('../../codex.json');
	process.stdout.write(`${JSON.stringify(agent_task_executors[0], null, 2)}\n`);
	process.exit(0);
}

const {
	runCliAgentTaskExecutorBin,
} = require('../../../lib/cli-agent-task-executor-bin');
const {
	executeCodexAgentTask,
	outcome,
	providerContract,
} = require('../../lib/codex-agent-task-executor');

runCliAgentTaskExecutorBin({
	execute: executeCodexAgentTask,
	outcome,
	providerContract,
});
