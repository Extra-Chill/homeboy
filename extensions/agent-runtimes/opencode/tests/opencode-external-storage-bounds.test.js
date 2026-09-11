'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { CONFIG_ENV, SCHEMA, finalizeOwnershipMarker, handleRequest, writeOwnershipMarker } = require('../lib/opencode-external-storage-retention');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'opencode-retention-bounds-'));
try {
	const temp = path.join(root, 'temp'); const data = path.join(root, 'data'); const nested = path.join(temp, 'a', 'b', 'terminal');
	fs.mkdirSync(nested, { recursive: true }); fs.mkdirSync(data); fs.writeFileSync(path.join(data, 'opencode.db'), 'db');
	const command = path.join(root, 'opencode.cjs');
	fs.writeFileSync(command, `#!/usr/bin/env node
const args = process.argv.slice(2).join(' ');
if (args === 'debug paths') process.stdout.write('tmp  ${temp}\\ndata  ${data}\\n');
else if (args === 'db path') process.stdout.write('${path.join(data, 'opencode.db')}\\n');
else if (args === 'session list --format json') process.stdout.write(JSON.stringify(Array.from({length: 6000}, (_, i) => ({id: 'session' + i, updated: 0}))));
`); fs.chmodSync(command, 0o755);
	const config = path.join(root, 'config.json'); fs.writeFileSync(config, JSON.stringify({ command, temp_roots: [temp], data_roots: [data] }));
	const env = { ...process.env, [CONFIG_ENV]: config, XDG_STATE_HOME: path.join(root, 'state') };
	assert.equal(writeOwnershipMarker(nested, { task_id: 'nested', workspace: root }, env), true);
	assert.equal(finalizeOwnershipMarker(nested, 'session5999', env), true);
	for (let index = 0; index < 10_001; index += 1) fs.writeFileSync(path.join(temp, `unknown-${index}`), Buffer.alloc(1));
	let current = path.join(temp, 'deep'); for (let index = 0; index < 34; index += 1) { current = path.join(current, 'd'); fs.mkdirSync(current, { recursive: true }); }
	fs.writeFileSync(path.join(current, 'payload'), 'x');
	const inventory = handleRequest({ schema: SCHEMA, operation: 'inventory' }, { env, now: Date.now() });
	assert.equal(inventory.items.some((item) => item.id === 'scratch:nested'), true, 'nested signed scratch is not starved by sessions');
	assert.equal(inventory.items.find((item) => item.id === 'scratch:nested').referenced, true, 'session links remain protected beyond the emitted session cap');
	assert.equal(inventory.completeness.complete, false);
	assert.ok(inventory.completeness.incomplete_roots.some((entry) => entry.reason === 'entry_limit'));
	assert.ok(inventory.completeness.incomplete_roots.some((entry) => entry.reason === 'depth_limit'));
	assert.ok(inventory.unknown_bytes < 10_001 + 1, 'bounded unknown byte total is explicitly a lower bound');
	const target = inventory.items.find((item) => item.id === 'scratch:nested');
	// Unrelated churn under the retention root no longer rejects a reclaim; the
	// session-referenced item below is protected on its own merits (#2832).
	fs.writeFileSync(path.join(temp, 'generation-change'), 'x');
	const afterChurn = handleRequest({ schema: SCHEMA, operation: 'reclaim', generation: inventory.generation, reclaim_targets: [{ id: target.id, reclaim_token: target.reclaim_token }] }, { env });
	assert.deepEqual(afterChurn.reclaimed_item_ids, [], 'a session-referenced item stays protected through unrelated root churn');
	assert.equal(fs.existsSync(nested), true, 'the referenced scratch directory stays on disk');
	fs.writeFileSync(config, JSON.stringify({ command, temp_roots: [temp, path.join(temp, 'a')], data_roots: [data] }));
	assert.throws(() => handleRequest({ schema: SCHEMA, operation: 'inventory' }, { env }), /same storage class/);
} finally { fs.rmSync(root, { recursive: true, force: true }); }
