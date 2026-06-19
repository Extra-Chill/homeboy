#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const ARTIFACT_DIR = path.join(process.cwd(), '.homeboy', 'fake-runtime');
const OUTCOME_PATH = path.join(ARTIFACT_DIR, 'outcome.json');
const TRANSCRIPT_PATH = path.join(ARTIFACT_DIR, 'transcript.log');

function readStdin() {
	return fs.readFileSync(0, 'utf8');
}

function emit(outcome) {
	writeArtifactFiles(outcome);
	process.stdout.write(`${JSON.stringify(outcome)}\n`);
}

function writeArtifactFiles(outcome) {
	fs.mkdirSync(ARTIFACT_DIR, { recursive: true });
	fs.writeFileSync(OUTCOME_PATH, `${JSON.stringify(outcome, null, 2)}\n`);
	fs.writeFileSync(TRANSCRIPT_PATH, transcriptForOutcome(outcome));
}

function transcriptForOutcome(outcome) {
	return [
		`task_id=${outcome.task_id || ''}`,
		`status=${outcome.status}`,
		`summary=${outcome.summary}`,
		'',
	].join('\n');
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
		artifacts: artifactRefs(),
		metadata: {
			provider: 'fake-runtime',
		},
	});
}

function artifactRefs() {
	return [
		{
			schema: 'homeboy/agent-task-artifact/v1',
			id: 'fake-runtime-outcome',
			kind: 'fake-runtime-outcome',
			name: 'Fake runtime outcome',
			path: '.homeboy/fake-runtime/outcome.json',
			mime: 'application/json',
		},
		{
			schema: 'homeboy/agent-task-artifact/v1',
			id: 'fake-runtime-transcript',
			kind: 'fake-runtime-transcript',
			name: 'Fake runtime transcript',
			path: '.homeboy/fake-runtime/transcript.log',
			mime: 'text/plain',
		},
	];
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
	artifacts: artifactRefs(),
	metadata: {
		provider: 'fake-runtime',
	},
});
