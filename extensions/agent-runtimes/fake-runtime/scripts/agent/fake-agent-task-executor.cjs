#!/usr/bin/env node
'use strict';

const fs = require('node:fs');

function readStdin() {
	return fs.readFileSync(0, 'utf8');
}

function emit(outcome) {
	process.stdout.write(`${JSON.stringify(outcome)}\n`);
}

function failure(taskId, summary, details) {
	emit({
		schema: 'homeboy/agent-task-outcome/v1',
		task_id: taskId || null,
		status: 'provider_error',
		summary,
		diagnostics: [
			{
				classification: 'request_validation',
				message: details,
			},
		],
		artifacts: [],
		metadata: {
			provider: 'fake-runtime',
		},
	});
}

let request;
try {
	request = JSON.parse(readStdin());
} catch (error) {
	failure(null, 'Invalid JSON request.', error.message);
	process.exit(0);
}

if (request.schema !== 'homeboy/agent-task-request/v1') {
	failure(request.task_id, 'Unsupported request schema.', `Received ${request.schema || 'missing schema'}.`);
	process.exit(0);
}

if (!request.task_id || request.executor?.backend !== 'fake-runtime' || !request.instructions) {
	failure(request.task_id, 'Request is missing required fake-runtime fields.', 'Expected task_id, executor.backend=fake-runtime, and instructions.');
	process.exit(0);
}

emit({
	schema: 'homeboy/agent-task-outcome/v1',
	task_id: request.task_id,
	status: 'succeeded',
	summary: 'Fake runtime accepted the request.',
	diagnostics: [
		{
			classification: 'provider',
			message: 'Provider command contract fixture executed successfully.',
		},
	],
	artifacts: [
		{
			kind: 'fake-runtime-diagnostic',
			path: '.homeboy/fake-runtime/outcome.json',
		},
	],
	metadata: {
		provider: 'fake-runtime',
		secret_env_names: process.env.FAKE_RUNTIME_TOKEN ? ['FAKE_RUNTIME_TOKEN'] : [],
	},
});
