'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createOpenCodeSessionActivity } = require('../lib/opencode-session-activity');

const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-session-activity-'));
try {
	const rootPath = path.join(directory, 'root.json');
	const filePath = path.join(directory, 'activity.jsonl');
	let clock = 1_000;
	const observe = createOpenCodeSessionActivity({ rootPath, filePath, directory, taskId: 'delegated-task', maxEvents: 2, now: () => clock++ });
	const session = (id, parentID, nativeDirectory = directory) => observe({ type: 'session.created', properties: { info: { id, parentID, directory: nativeDirectory } } });
	const tool = (id, partId, status = 'completed') => observe({ type: 'message.part.updated', properties: { part: { id: partId, sessionID: id, type: 'tool', tool: 'read', state: { status, input: { filePath: '/private/secret' }, output: 'PRIVATE_TOKEN', error: 'private failure' } } } });
	const records = () => fs.readFileSync(filePath, 'utf8').trim().split('\n').map(JSON.parse);

	// Child registration can precede the first CLI frame that identifies its
	// parent. It must not keep anything alive until the exact root is pinned.
	session('ses_child', 'ses_root');
	tool('ses_child', 'part_early');
	assert.equal(fs.existsSync(filePath), false);
	fs.writeFileSync(rootPath, JSON.stringify({ session_id: 'ses_root' }));
	tool('ses_child', 'part_1');
	assert.equal(records()[0].session_id, 'ses_child');
	assert.equal(records()[0].type, 'file.read.completed');
	assert.equal(fs.readFileSync(filePath, 'utf8').includes('PRIVATE_TOKEN'), false);
	assert.equal(fs.readFileSync(filePath, 'utf8').includes('/private/secret'), false);

	const first = fs.readFileSync(filePath, 'utf8');
	session('ses_unrelated', 'ses_other_root');
	tool('ses_unrelated', 'part_unrelated');
	session('ses_wrong_directory', 'ses_root', path.dirname(directory));
	tool('ses_wrong_directory', 'part_wrong_directory');
	observe({ type: 'session.status', properties: { sessionID: 'ses_child', status: { type: 'busy' } } });
	assert.equal(fs.readFileSync(filePath, 'utf8'), first, 'unrelated/busy status is not progress');
	tool('ses_child', 'part_1');
	assert.equal(fs.readFileSync(filePath, 'utf8'), first, 'replayed transitions are not progress');

	session('ses_grandchild', 'ses_child');
	tool('ses_grandchild', 'part_2', 'running');
	tool('ses_grandchild', 'part_2', 'completed');
	tool('ses_grandchild', 'part_3');
	assert.equal(records().length, 2, 'evidence remains bounded');
	assert.equal(records().at(-1).sequence, 4, 'activity keeps advancing beyond retention ceiling');
	assert.equal(records().at(-1).session_id, 'ses_grandchild');
	clock = 3_000;
	observe({ type: 'message.part.delta', properties: { sessionID: 'ses_child', field: 'reasoning', delta: 'PRIVATE_REASONING' } });
	assert.equal(records().at(-1).type, 'provider.activity');
	assert.equal(fs.readFileSync(filePath, 'utf8').includes('PRIVATE_REASONING'), false);

	const beforeFailure = fs.readFileSync(filePath, 'utf8');
	fs.writeFileSync(rootPath, 'invalid');
	tool('ses_child', 'part_4');
	assert.equal(fs.readFileSync(filePath, 'utf8'), beforeFailure, 'unknown ownership fails closed');
	console.log('OpenCode owned session activity: lineage, negative controls, payload bounds and retention verified');
} finally {
	fs.rmSync(directory, { recursive: true, force: true });
}
