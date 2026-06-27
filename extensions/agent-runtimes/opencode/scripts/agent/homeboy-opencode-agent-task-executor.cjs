#!/usr/bin/env node
'use strict';

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
