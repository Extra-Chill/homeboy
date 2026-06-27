#!/usr/bin/env node
'use strict';

const {
	runCliAgentTaskExecutorBin,
} = require('../../../lib/cli-agent-task-executor-bin');
const {
	executePiAgentTask,
	outcome,
	providerContract,
} = require('../../lib/pi-agent-task-executor');

runCliAgentTaskExecutorBin({
	execute: executePiAgentTask,
	outcome,
	providerContract,
});
