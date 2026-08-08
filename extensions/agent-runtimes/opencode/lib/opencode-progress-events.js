'use strict';

const fs = require('node:fs');
const path = require('node:path');

const OPENCODE_PROGRESS_EVENT_SCHEMA = 'homeboy/agent-task-progress/v1';
const DEFAULT_MAX_PROGRESS_EVENTS = 200;
const MAX_SUMMARY_LENGTH = 240;

function createOpenCodeProgressAdapter(options = {}) {
	const state = { sequence: 0, emitted: [], dropped: 0, lastSignature: '', buffers: { stdout: '', stderr: '' } };
	const maxEvents = positiveInteger(options.maxEvents, DEFAULT_MAX_PROGRESS_EVENTS);
	const workspace = options.cwd && path.isAbsolute(options.cwd) ? path.resolve(options.cwd) : '';
	if (options.filePath) {
		fs.mkdirSync(path.dirname(options.filePath), { recursive: true });
		fs.writeFileSync(options.filePath, '');
	}
	const emit = (event) => {
		const signature = JSON.stringify({ type: event.type, data: event.data });
		if (signature === state.lastSignature || state.emitted.length >= maxEvents) {
			state.dropped += 1;
			return false;
		}
		state.lastSignature = signature;
		state.sequence += 1;
		event.sequence = state.sequence;
		event.cursor = `opencode:${state.sequence}`;
		state.emitted.push(event);
		if (options.filePath) {
			fs.appendFileSync(options.filePath, `${JSON.stringify(event)}\n`);
		}
		try {
			options.onProgress?.(event);
		} catch {
			// Progress delivery is advisory and must not terminate provider execution.
		}
		return true;
	};
	const consume = (stream, chunk) => {
		state.buffers[stream] = `${state.buffers[stream] || ''}${String(chunk || '')}`;
		const lines = state.buffers[stream].split(/\r?\n/);
		state.buffers[stream] = lines.pop();
		for (const line of lines) consumeLine(line, stream, emit, options, workspace, state);
	};
	const finish = () => {
		for (const [stream, line] of Object.entries(state.buffers)) {
			if (line) consumeLine(line, stream, emit, options, workspace, state);
		}
		state.buffers = { stdout: '', stderr: '' };
		return summary(state);
	};
	return { consume, finish, events: () => [...state.emitted], summary: () => summary(state) };
}

function consumeLine(line, stream, emit, options, workspace, state) {
	let frame;
	try {
		frame = JSON.parse(line);
	} catch {
		return;
	}
	const event = translateOpenCodeEvent(frame, { stream, taskId: options.taskId, workspace, env: options.env, now: options.now });
	if (!event) return;
	emit(event);
}

function translateOpenCodeEvent(frame = {}, context = {}) {
	const parts = Array.isArray(frame.parts) ? frame.parts : [frame];
	for (const part of parts) {
		const tool = stringValue(part.tool || part.name || frame.tool || frame.name);
		const input = objectValue(part.input || part.args || frame.input || frame.args);
		if (!tool) continue;
		const status = stringValue(part.state?.status || part.status || frame.status).toLowerCase();
		const failed = Boolean(part.state?.error || part.error || frame.error) || ['error', 'failed'].includes(status);
		const completed = ['completed', 'complete', 'done', 'success', 'succeeded'].includes(status);
		const type = progressType(tool, failed ? 'failed' : completed ? 'completed' : 'started');
		return progressEnvelope(type, tool, input, frame, context, failed ? part.state?.error || part.error || frame.error : '', sessionId(part, frame));
	}
	const error = stringValue(frame.error || frame.message);
	if (error && /(retry|rate limit|quota|backoff)/i.test(error)) {
		return progressEnvelope('provider.retrying', '', {}, frame, context, error, sessionId({}, frame));
	}
	return null;
}

function progressType(tool, state) {
	const normalized = tool.toLowerCase();
	if (['bash', 'command', 'shell', 'execute'].includes(normalized)) return `command.${state}`;
	if (['read', 'cat'].includes(normalized)) return `file.read.${state}`;
	if (['glob', 'grep', 'search', 'find'].includes(normalized)) return `file.search.${state}`;
	if (['edit', 'write', 'apply_patch', 'patch', 'delete'].includes(normalized)) return `file.edit.${state}`;
	return `tool.${state}`;
}

function progressEnvelope(type, tool, input, frame, context, error, session_id) {
	const data = {};
	if (tool) data.tool = bounded(sanitize(tool, context), 80);
	const candidatePath = stringValue(input.path || input.filePath || input.filepath || input.file || input.pattern);
	if (candidatePath) data.path = workspaceRelativePath(candidatePath, context.workspace);
	const command = stringValue(input.command || input.cmd);
	if (command) data.command = bounded(sanitizeCommand(command, context), MAX_SUMMARY_LENGTH);
	if (input.query) data.query = bounded(sanitize(String(input.query), context), 120);
	if (error) data.message = bounded(sanitize(String(error), context), MAX_SUMMARY_LENGTH);
	const exitCode = Number(input.exit_code ?? input.exitCode ?? frame.exit_code ?? frame.exitCode);
	if (Number.isInteger(exitCode)) data.exit_code = exitCode;
	return {
		schema: OPENCODE_PROGRESS_EVENT_SCHEMA,
		task_id: context.taskId || 'unknown-task',
		session_id,
		source: 'provider',
		type,
		timestamp: validTimestamp(frame.timestamp || frame.time?.created || frame.created_at) || timestamp(context.now),
		data,
	};
}

function sessionId(part, frame) {
	const value = stringValue(
		part.session_id || part.sessionID || part.session?.id
		|| frame.session_id || frame.sessionID || frame.session?.id
	);
	return /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value) ? value : 'unknown';
}

function workspaceRelativePath(value, workspace) {
	const sanitized = sanitize(value, {});
	if (!path.isAbsolute(sanitized)) return bounded(sanitized.replaceAll('\\', '/'), MAX_SUMMARY_LENGTH);
	if (workspace && (sanitized === workspace || sanitized.startsWith(`${workspace}${path.sep}`))) {
		return path.relative(workspace, sanitized).replaceAll('\\', '/') || '.';
	}
	return '<private-path>';
}

function sanitizeCommand(value, context) {
	return sanitize(value, context)
		.replace(/\b[A-Za-z_][A-Za-z0-9_]*(?:TOKEN|SECRET|KEY|PASSWORD|AUTH)=[^\s]+/gi, (match) => `${match.split('=')[0]}=[redacted]`)
		.replace(/\/[A-Za-z0-9_./~-]+/g, (candidate) => workspaceRelativePath(candidate, context.workspace));
}

function sanitize(value, context) {
	let result = String(value || '');
	for (const secret of Object.values(context.env || {})) {
		if (typeof secret === 'string' && secret.length >= 4) result = result.split(secret).join('[redacted]');
	}
	return result.replace(/([?&](?:token|key|secret|password|authorization)=)[^&#\s]*/gi, '$1[redacted]');
}

function summary(state) {
	return { emitted: state.emitted.length, coalesced_or_dropped: state.dropped, last_type: state.emitted.at(-1)?.type || '' };
}

function validTimestamp(value) { return typeof value === 'string' && !Number.isNaN(Date.parse(value)) ? value : ''; }
function timestamp(value) { return typeof value === 'function' ? value() : typeof value === 'string' ? value : new Date().toISOString(); }
function bounded(value, max) { return String(value || '').slice(0, max); }
function stringValue(value) { return typeof value === 'string' && value.trim() ? value.trim() : ''; }
function objectValue(value) { return value && typeof value === 'object' && !Array.isArray(value) ? value : {}; }
function positiveInteger(value, fallback) { return Number.isInteger(Number(value)) && Number(value) > 0 ? Number(value) : fallback; }

module.exports = { OPENCODE_PROGRESS_EVENT_SCHEMA, createOpenCodeProgressAdapter, translateOpenCodeEvent };
