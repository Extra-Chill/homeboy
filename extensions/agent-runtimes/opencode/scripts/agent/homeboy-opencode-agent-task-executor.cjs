#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

const fs = require('node:fs');
const {
	executeOpenCodeAgentTask,
	outcome,
	providerContract,
} = require('../../lib/opencode-agent-task-executor');

function readStdin() {
	try {
		return fs.readFileSync(0, 'utf8');
	} catch {
		return '';
	}
}

if (process.argv.includes('--provider-contract')) {
	process.stdout.write(`${JSON.stringify(providerContract(), null, 2)}\n`);
	process.exit(0);
}

let request = {};
const raw = process.env.HOMEBOY_AGENT_TASK_REQUEST || readStdin();
if (raw.trim()) {
	try {
		request = JSON.parse(raw);
	} catch (error) {
		process.stdout.write(`${JSON.stringify(outcome({}, {
			status: 'provider_error',
			failure_classification: 'invalid_input',
			failure_code: 'agent_task.invalid_json',
			summary: 'Invalid AgentTaskRequest JSON.',
			diagnostics: [{ classification: 'request_validation', message: error.message }],
		}), null, 2)}\n`);
		process.exit(0);
	}
}

process.stdout.write(`${JSON.stringify(executeOpenCodeAgentTask(request), null, 2)}\n`);
process.exit(0);
