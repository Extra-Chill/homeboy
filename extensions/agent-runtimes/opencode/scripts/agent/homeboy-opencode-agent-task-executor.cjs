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
	onProgress: relayProgress,
});

function relayProgress(event = {}) {
	const progress = {
		schema: 'homeboy/agent-task-runtime-progress/v1',
		provider: 'opencode',
		session_id: safeSessionId(event.session_id),
		category: safeCategory(event.type),
		latest_activity_at: safeTimestamp(event.timestamp),
	};
	try {
		// stderr preserves stdout as the terminal AgentTaskOutcome channel.
		process.stderr.write(`${JSON.stringify(progress)}\n`);
	} catch {
		// Parent progress is advisory; a closed parent pipe must not end the task.
	}
}

function safeSessionId(value) {
	return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value) ? value : 'unknown';
}

function safeCategory(value) {
	return typeof value === 'string' && /^(?:command|file\.(?:read|search|edit)|tool)\.(?:started|completed|failed)$|^provider\.retrying$/.test(value)
		? value
		: 'provider.activity';
}

function safeTimestamp(value) {
	return typeof value === 'string' && !Number.isNaN(Date.parse(value)) ? value : new Date().toISOString();
}
