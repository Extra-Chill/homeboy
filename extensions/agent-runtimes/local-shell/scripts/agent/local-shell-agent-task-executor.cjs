#!/usr/bin/env node
'use strict';

const { spawnSync } = require('node:child_process');
const fs = require('node:fs');

const REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const OUTCOME_SCHEMA = 'homeboy/agent-task-outcome/v1';
const BACKEND = 'local-shell';
const DEFAULT_TIMEOUT_SECONDS = 300;

function emit(outcome) {
	process.stdout.write(`${JSON.stringify(outcome)}\n`);
}

function diagnostic(classification, message, extra = {}) {
	return { classification, message, ...extra };
}

function outcome({ taskId, status, summary, diagnostics, metadata = {} }) {
	return {
		schema: OUTCOME_SCHEMA,
		task_id: taskId || null,
		status,
		summary,
		diagnostics,
		artifacts: [],
		metadata: {
			provider: BACKEND,
			...metadata,
		},
	};
}

function providerError(taskId, summary, message) {
	emit(outcome({
		taskId,
		status: 'provider_error',
		summary,
		diagnostics: [diagnostic('request_validation', message)],
	}));
}

function normalizeStringArray(value) {
	return Array.isArray(value) ? value.filter((entry) => typeof entry === 'string') : [];
}

function normalizeEnv(value) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return {};
	}

	return Object.fromEntries(
		Object.entries(value).filter(([key, envValue]) => typeof key === 'string' && typeof envValue === 'string')
	);
}

let request;
try {
	request = JSON.parse(fs.readFileSync(0, 'utf8'));
} catch (error) {
	providerError(null, 'Invalid JSON request.', error.message);
	process.exit(0);
}

if (request.schema !== REQUEST_SCHEMA) {
	providerError(request.task_id, 'Unsupported request schema.', `Received ${request.schema || 'missing schema'}.`);
	process.exit(0);
}

if (!request.task_id || request.executor?.backend !== BACKEND || !request.instructions) {
	providerError(request.task_id, 'Request is missing required local-shell fields.', 'Expected task_id, executor.backend=local-shell, and instructions.');
	process.exit(0);
}

const config = request.executor.config || {};
if (!config.command || typeof config.command !== 'string') {
	providerError(request.task_id, 'Request is missing a local command.', 'Expected executor.config.command to be a non-empty string.');
	process.exit(0);
}

const args = normalizeStringArray(config.args);
const cwd = typeof config.cwd === 'string' && config.cwd ? config.cwd : process.cwd();
const timeoutSeconds = Number.isFinite(config.timeout_seconds) ? config.timeout_seconds : DEFAULT_TIMEOUT_SECONDS;
const timeoutMs = Math.max(1, timeoutSeconds) * 1000;
const env = { ...process.env, ...normalizeEnv(config.env) };

const result = spawnSync(config.command, args, {
	cwd,
	env,
	encoding: 'utf8',
	timeout: timeoutMs,
});

if (result.error?.code === 'ETIMEDOUT') {
	emit(outcome({
		taskId: request.task_id,
		status: 'timeout',
		summary: 'Local shell command timed out.',
		diagnostics: [diagnostic('timeout', `Command exceeded ${timeoutSeconds} seconds.`)],
		metadata: { command: config.command, args_count: args.length, cwd },
	}));
	process.exit(0);
}

if (result.error) {
	emit(outcome({
		taskId: request.task_id,
		status: 'provider_error',
		summary: 'Local shell command could not be started.',
		diagnostics: [diagnostic('provider', result.error.message)],
		metadata: { command: config.command, args_count: args.length, cwd },
	}));
	process.exit(0);
}

const status = result.status === 0 ? 'succeeded' : 'failed';
emit(outcome({
	taskId: request.task_id,
	status,
	summary: status === 'succeeded' ? 'Local shell command completed successfully.' : 'Local shell command failed.',
	diagnostics: [diagnostic(status === 'succeeded' ? 'provider' : 'execution_failed', `Command exited with status ${result.status}.`, { exit_code: result.status })],
	metadata: { command: config.command, args_count: args.length, cwd },
}));
