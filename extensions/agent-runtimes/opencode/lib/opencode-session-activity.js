'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { translateOpenCodeEvent, OPENCODE_PROGRESS_EVENT_SCHEMA } = require('./opencode-progress-events');

// OpenCode's CLI only streams the root session. This observer runs at the
// native event boundary so delegated work remains visible to the same watchdog.
function createOpenCodeSessionActivity({ rootPath, filePath, taskId, directory, now = Date.now, maxEvents = 200 }) {
	const parents = new Map();
	const states = new Map();
	const events = [];
	let sequence = 0;
	let lastDeltaAt = 0;
	const validId = (value) => typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value);
	const rootId = () => {
		try { return JSON.parse(fs.readFileSync(rootPath, 'utf8')).session_id; } catch { return undefined; }
	};
	const owned = (id) => {
		const root = rootId();
		if (!validId(root)) return false;
		const seen = new Set();
		for (let current = id; validId(current) && !seen.has(current) && seen.size < 64; current = parents.get(current)) {
			if (current === root) return true;
			seen.add(current);
		}
		return false;
	};
	return (event) => {
		try {
			const properties = event?.properties || {};
			if (event?.type === 'session.created' || event?.type === 'session.updated') {
				const info = properties.info;
				if (validId(info?.id) && validId(info.parentID) && path.resolve(info.directory || '') === path.resolve(directory) && parents.size < 4096) {
					parents.set(info.id, info.parentID);
				}
				return;
			}
			const part = properties.part;
			const id = part?.sessionID || properties.sessionID;
			if (!validId(id) || !owned(id)) return;
			let progress;
			if (event.type === 'message.part.updated' && part?.type === 'tool') {
				const signature = `${part.id}:${part.state?.status}`;
				if (!validId(part.id) || states.get(part.id) === signature) return;
				if (states.size >= 4096) states.delete(states.keys().next().value);
				states.set(part.id, signature);
				// Inputs, outputs and error text stay in the native transcript, not
				// in the liveness signal. Only the typed tool transition is needed.
				progress = translateOpenCodeEvent({ sessionID: id, part: { tool: part.tool, state: { status: part.state?.status } } }, { taskId });
			} else if (event.type === 'message.part.delta' && ['text', 'reasoning'].includes(properties.field)) {
				if (now() - lastDeltaAt < 1000) return;
				lastDeltaAt = now();
				progress = { schema: OPENCODE_PROGRESS_EVENT_SCHEMA, task_id: taskId, session_id: id, source: 'provider', type: 'provider.activity', data: { event: event.type } };
			}
			if (!progress) return;
			progress.timestamp = new Date(now()).toISOString();
			progress.sequence = ++sequence;
			progress.cursor = `opencode-events:${sequence}`;
			events.push(progress);
			if (events.length > maxEvents) events.shift();
			// Bounded retained evidence keeps advancing even after the retention
			// ceiling; reaching that ceiling must not look like a stalled task.
			fs.writeFileSync(filePath, events.map((entry) => JSON.stringify(entry)).join('\n') + '\n');
		} catch {
			// Advisory telemetry cannot interrupt the agent or leak its payload.
		}
	};
}

module.exports = { createOpenCodeSessionActivity };
