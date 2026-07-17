#!/usr/bin/env node
'use strict';

if (process.argv.includes('--provider-contract')) {
	const { agent_task_executors } = require('../../claude-code.json');
	process.stdout.write(`${JSON.stringify(agent_task_executors[0], null, 2)}\n`);
	process.exit(0);
}

const {
	runCliAgentTaskExecutorBin,
} = require('../../../lib/cli-agent-task-executor-bin');
const {
	executeClaudeCodeAgentTask,
	outcome,
	providerContract,
} = require('../../lib/claude-code-agent-task-executor');

runCliAgentTaskExecutorBin({
	execute: executeClaudeCodeAgentTask,
	outcome,
	providerContract,
});
