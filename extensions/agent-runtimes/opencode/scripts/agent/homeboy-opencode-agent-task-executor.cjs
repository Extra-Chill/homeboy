#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

const fs = require('node:fs');
const {
	experimentalOutcome,
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
		console.error(`Invalid AgentTaskRequest JSON: ${error.message}`);
		process.exit(1);
	}
}

process.stdout.write(`${JSON.stringify(experimentalOutcome(request), null, 2)}\n`);
process.exit(1);
