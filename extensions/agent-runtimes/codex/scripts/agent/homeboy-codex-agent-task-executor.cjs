#!/usr/bin/env node
'use strict';

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
