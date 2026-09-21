'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { CONFIG_ENV, SCHEMA, handleRequest } = require('../lib/opencode-external-storage-retention');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'opencode-maintenance-state-'));
try {
	const command = path.join(root, 'native-fixture.cjs');
	const state = path.join(root, 'state');
	const dbA = path.join(root, 'db-a', 'opencode.db');
	const dbB = path.join(root, 'db-b', 'opencode.db');
	for (const database of [dbA, dbB]) fs.mkdirSync(path.dirname(database), { recursive: true });
	fs.writeFileSync(dbA, 'a');
	fs.writeFileSync(dbB, 'b');
	fs.writeFileSync(command, `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
const db = process.env.FIXTURE_DB;
const log = db + '.args';
if (args.join(' ') === 'debug paths') process.stdout.write('tmp  ' + process.env.FIXTURE_TMP + '\\ndata  ' + path.dirname(db) + '\\n');
else if (args.join(' ') === 'db path') process.stdout.write(db + '\\n');
else if (args.join(' ') === 'session list --format json') process.stdout.write('[]');
else if (args.join(' ') === 'db event-log-status') process.stdout.write(JSON.stringify({events:2,payloadBytes:200,compactableEvents:2,recommended:true}));
else if (args[0] === 'db' && args[1] === 'compact-events' && args.includes('--apply')) {
  fs.appendFileSync(log, args.join(' ') + '\\n');
  const count = Number(fs.existsSync(db + '.count') ? fs.readFileSync(db + '.count', 'utf8') : '0') + 1; fs.writeFileSync(db + '.count', String(count));
  if (count === 2 && process.env.FAIL_AFTER_FIRST === '1') { process.exitCode = 1; }
  else process.stdout.write(JSON.stringify({contract:'opencode.db.compact-events.v1',capabilities:{replaySafe:'supported',interruptionResume:'supported',physicalReclamation:'not-requested'},dryRun:false,inspected:2,candidates:1,rewritten:1,payloadBytesReclaimed:80,bytes:{logicalPayloadReclaimed:80,physicalReclaimed:null},...(count === 1 ? {next:{cursor:'db-a',afterSeq:3}} : {})}));
}
`);
	fs.chmodSync(command, 0o755);
	const config = path.join(root, 'retention.json');
	fs.writeFileSync(config, JSON.stringify({ command, temp_roots: [], data_roots: [], operation_timeout_ms: 10000 }));
	const baseEnv = { ...process.env, [CONFIG_ENV]: config, XDG_STATE_HOME: state };
	const target = (inventory) => {
		const item = inventory.items.find((entry) => entry.id.startsWith('compaction:'));
		assert.ok(item);
		return { id: item.id, reclaim_token: item.reclaim_token };
	};
const run = (database, extra = {}) => ({ ...baseEnv, FIXTURE_DB: database, FIXTURE_TMP: path.join(root, path.basename(path.dirname(database), 'db') + '-temp'), ...extra });

	const first = handleRequest({ schema: SCHEMA, operation: 'inventory' }, { env: run(dbA) });
	const failed = handleRequest({ schema: SCHEMA, operation: 'reclaim', generation: first.generation, reclaim_targets: [target(first)] }, { env: run(dbA, { FAIL_AFTER_FIRST: '1' }) });
	assert.deepEqual(failed.reclaimed_item_ids, []);
	const resumedInventory = handleRequest({ schema: SCHEMA, operation: 'inventory' }, { env: run(dbA) });
	const resumed = handleRequest({ schema: SCHEMA, operation: 'reclaim', generation: resumedInventory.generation, reclaim_targets: [target(resumedInventory)] }, { env: run(dbA) });
	assert.deepEqual(resumed.reclaimed_item_ids, [target(resumedInventory).id]);
	assert.match(fs.readFileSync(`${dbA}.args`, 'utf8'), /--cursor db-a --after-seq 3/);

	const second = handleRequest({ schema: SCHEMA, operation: 'inventory' }, { env: run(dbB) });
	const secondReceipt = handleRequest({ schema: SCHEMA, operation: 'reclaim', generation: second.generation, reclaim_targets: [target(second)] }, { env: run(dbB) });
	assert.deepEqual(secondReceipt.reclaimed_item_ids, [target(second).id]);
	assert.doesNotMatch(fs.readFileSync(`${dbB}.args`, 'utf8').split('\n')[0], /--cursor/);
	console.log('opencode maintenance state isolation: ok');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
