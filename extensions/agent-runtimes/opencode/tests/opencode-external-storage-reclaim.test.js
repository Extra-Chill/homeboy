'use strict';

// Reclaim must survive a retention root that keeps being written by normal
// agent activity between inventory and reclaim (#2832), while still refusing a
// target whose own directory changed since it was inventoried.

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { CONFIG_ENV, SCHEMA, finalizeOwnershipMarker, handleRequest, writeOwnershipMarker } = require('../lib/opencode-external-storage-retention');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'opencode-retention-reclaim-'));
try {
	const temp = path.join(root, 'temp');
	const data = path.join(root, 'data');
	fs.mkdirSync(temp, { recursive: true });
	fs.mkdirSync(data);
	fs.writeFileSync(path.join(data, 'opencode.db'), 'db');

	const command = path.join(root, 'opencode.cjs');
	fs.writeFileSync(command, `#!/usr/bin/env node
const args = process.argv.slice(2).join(' ');
if (args === 'debug paths') process.stdout.write('tmp  ${temp}\\ndata  ${data}\\n');
else if (args === 'db path') process.stdout.write('${path.join(data, 'opencode.db')}\\n');
else if (args === 'session list --format json') process.stdout.write('[]');
`);
	fs.chmodSync(command, 0o755);

	const config = path.join(root, 'config.json');
	fs.writeFileSync(config, JSON.stringify({ command, temp_roots: [temp], data_roots: [data] }));
	const env = { ...process.env, [CONFIG_ENV]: config, XDG_STATE_HOME: path.join(root, 'state') };

	// Two signed, terminal, unreferenced scratch directories: both reclaimable.
	const churned = path.join(temp, 'churned');
	const mutated = path.join(temp, 'mutated');
	for (const [directory, id] of [[churned, 'churned'], [mutated, 'mutated']]) {
		fs.mkdirSync(directory, { recursive: true });
		fs.writeFileSync(path.join(directory, 'payload'), 'x');
		assert.equal(writeOwnershipMarker(directory, { task_id: id, workspace: root }, env), true);
		assert.equal(finalizeOwnershipMarker(directory, `session-${id}`, env), true);
	}

	const inventory = handleRequest({ schema: SCHEMA, operation: 'inventory' }, { env, now: Date.now() });
	const target = (id) => {
		const item = inventory.items.find((entry) => entry.id === `scratch:${id}`);
		assert.ok(item, `${id} is inventoried`);
		return { id: item.id, reclaim_token: item.reclaim_token };
	};
	const churnedTarget = target('churned');
	const mutatedTarget = target('mutated');

	// An empty reclaim is a valid request that mutates nothing.
	const empty = handleRequest(
		{ schema: SCHEMA, operation: 'reclaim', generation: inventory.generation, reclaim_targets: [] },
		{ env, now: Date.now() },
	);
	assert.deepEqual(empty.reclaimed_item_ids, [], 'an empty reclaim reclaims nothing');
	assert.equal(empty.reclaimed_bytes, 0);

	// Unrelated activity under the retention root: a new sibling directory and
	// a new file both bump the root's own mtime.
	fs.mkdirSync(path.join(temp, 'unrelated-new-work'), { recursive: true });
	fs.writeFileSync(path.join(temp, 'unrelated-file'), 'y');
	// The mutated target's own contents change after it was inventoried.
	fs.writeFileSync(path.join(mutated, 'late-write'), 'z');

	const result = handleRequest(
		{
			schema: SCHEMA,
			operation: 'reclaim',
			generation: inventory.generation,
			reclaim_targets: [churnedTarget, mutatedTarget],
		},
		{ env, now: Date.now() },
	);

	assert.deepEqual(
		result.reclaimed_item_ids,
		['scratch:churned'],
		'unrelated root churn does not block reclaim, and a mutated target is refused',
	);
	assert.equal(
		result.generation,
		inventory.generation,
		'the receipt echoes the generation the reclaim was requested against, which Homeboy validates',
	);
	assert.equal(fs.existsSync(churned), false, 'the reclaimed directory is removed');
	assert.equal(fs.existsSync(mutated), true, 'the mutated target is left on disk');
	assert.ok(result.reclaimed_bytes > 0, 'reclaimed bytes are reported');

	console.log('opencode external storage reclaim: ok');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
