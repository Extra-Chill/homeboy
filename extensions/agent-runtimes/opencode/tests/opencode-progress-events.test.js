'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createOpenCodeProgressAdapter } = require('../lib/opencode-progress-events');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opencode-progress-'));
try {
	const workspace = path.join(root, 'workspace');
	const eventPath = path.join(root, 'progress.jsonl');
	fs.mkdirSync(workspace);
	const live = [];
	const adapter = createOpenCodeProgressAdapter({
		taskId: 'task-2311', cwd: workspace, filePath: eventPath,
		env: { ACCESS_TOKEN: 'top-secret-token' }, now: '2026-07-20T12:00:00.000Z', onProgress: (event) => live.push(event),
	});
	adapter.consume('stdout', `${JSON.stringify({ sessionID: 'ses_123', timestamp: '2026-07-20T12:00:01.000Z', parts: [{ tool: 'read', input: { path: path.join(workspace, 'src/index.js') }, state: { status: 'completed' } }] })}\n`);
	adapter.consume('stdout', `${JSON.stringify({ parts: [{ tool: 'bash', input: { command: `ACCESS_TOKEN=top-secret-token npm test ${path.join(workspace, 'package.json')}` }, state: { status: 'completed', exit_code: 0 } }] })}\n`);
	adapter.consume('stdout', `${JSON.stringify({ type: 'tool_use', sessionID: 'ses_current', timestamp: 1787763877191, part: { type: 'tool', tool: 'grep', state: { status: 'completed', input: { pattern: 'current-shape' } } } })}\n`);
	adapter.consume('stderr', `${JSON.stringify({ error: 'Rate limit reached; retrying with token top-secret-token.' })}\n`);
	const summary = adapter.finish();
	assert.deepEqual(live, JSON.parse(`[${fs.readFileSync(eventPath, 'utf8').trim().split('\n').join(',')}]`));
	assert.deepEqual(live.map((event) => event.type), ['file.read.completed', 'command.completed', 'file.search.completed', 'provider.retrying']);
	assert.deepEqual(live.map((event) => event.sequence), [1, 2, 3, 4]);
	assert.deepEqual(live.map((event) => event.cursor), ['opencode:1', 'opencode:2', 'opencode:3', 'opencode:4']);
	assert.equal(live[0].session_id, 'ses_123');
	assert.equal(live[0].data.path, 'src/index.js');
	assert.equal(live[1].data.command.includes('top-secret-token'), false);
	assert.equal(live[1].data.command.includes('ACCESS_TOKEN=[redacted]'), true);
	assert.equal(live[1].data.command.includes('workspace/package.json'), false);
	assert.equal(live[1].data.command.includes('package.json'), true);
	assert.equal(live[2].session_id, 'ses_current');
	assert.equal(live[2].data.path, 'current-shape');
	assert.equal(live[2].timestamp, '2026-08-26T17:04:37.191Z');
	assert.equal(JSON.stringify(live).includes(root), false);
	assert.equal(JSON.stringify(live).includes('top-secret-token'), false);
	assert.deepEqual(summary, { emitted: 4, coalesced_or_dropped: 0, last_type: 'provider.retrying' });

	const bounded = createOpenCodeProgressAdapter({ taskId: 'task-2311', cwd: workspace, maxEvents: 2, now: '2026-07-20T12:00:00.000Z' });
	for (const pathname of ['a.js', 'a.js', 'b.js', 'c.js']) {
		bounded.consume('stdout', `${JSON.stringify({ parts: [{ tool: 'read', input: { path: pathname } }] })}\n`);
	}
	assert.deepEqual(bounded.events().map((event) => event.data.path), ['a.js', 'b.js']);
	assert.deepEqual(bounded.finish(), { emitted: 2, coalesced_or_dropped: 2, last_type: 'file.read.started' });
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('OpenCode progress event replay passed\n');
