#!/usr/bin/env node
'use strict';

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
