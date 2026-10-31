'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const fs = require('node:fs');

/**
 * Drive a CLI agent-task executor from a standalone runtime bin.
 *
 * Every per-provider `homeboy-<runtime>-agent-task-executor.cjs` wrapper shares
 * the same stdin/argv contract: print the provider contract for
 * `--provider-contract`, otherwise read an AgentTaskRequest from
 * HOMEBOY_AGENT_TASK_REQUEST or stdin, parse it, and emit a normalized
 * AgentTaskOutcome. This helper centralizes that contract so each runtime bin is
 * a one-line invocation with its own executor module.
 *
 * @param {{execute: Function, outcome: Function, providerContract: Function}} executor Provider executor surface.
 */
async function runCliAgentTaskExecutorBin({ execute, outcome, providerContract, onProgress }) {
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

	process.stdout.write(`${JSON.stringify(await execute(request, { onProgress }), null, 2)}\n`);
	process.exit(0);
}

function readStdin() {
	try {
		return fs.readFileSync(0, 'utf8');
	} catch {
		return '';
	}
}

module.exports = {
	runCliAgentTaskExecutorBin,
};
