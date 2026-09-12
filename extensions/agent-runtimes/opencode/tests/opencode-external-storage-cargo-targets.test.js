'use strict';

// Idle Cargo target directories under a provider temp root are reclaimable
// scratch (#2838). Identification is by Cargo's own CACHEDIR.TAG signature and
// never by directory name, and git-tracked content is never disposable.

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { CONFIG_ENV, SCHEMA, handleRequest } = require('../lib/opencode-external-storage-retention');

const CARGO_SIGNATURE = 'Signature: 8a477f597d28d172789f06886806bc55';
const DAY = 86_400_000;

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'opencode-retention-cargo-'));
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

	const aged = (directory, days) => {
		const when = new Date(Date.now() - days * DAY);
		for (const entry of [path.join(directory, 'debug'), path.join(directory, 'CACHEDIR.TAG'), directory]) {
			try { fs.utimesSync(entry, when, when); } catch { /* optional profile */ }
		}
	};
	const cargoTarget = (name, { signature = CARGO_SIGNATURE, days = 30, payload = 'artifact' } = {}) => {
		const directory = path.join(temp, name);
		fs.mkdirSync(path.join(directory, 'debug'), { recursive: true });
		fs.writeFileSync(path.join(directory, 'CACHEDIR.TAG'), `${signature}\n`);
		fs.writeFileSync(path.join(directory, 'debug', 'binary'), payload);
		aged(directory, days);
		return directory;
	};

	// Idle, reproducible Cargo output: the case this feature exists for.
	const idle = cargoTarget('idle-target');
	// A live build: Cargo holds .cargo-lock for the duration.
	const building = cargoTarget('building-target');
	fs.writeFileSync(path.join(building, '.cargo-lock'), '');
	// Recently written output is not idle regardless of the lock.
	const recent = cargoTarget('recent-target', { days: 0 });
	// Looks like build output, is not stamped by Cargo.
	const unsigned = cargoTarget('unsigned-target', { signature: 'Signature: something-else' });
	// The regression case: a git-tracked fixture that lives under a directory
	// literally named `target` and carries a real Cargo tag.
	const repo = path.join(temp, 'fixture-repo');
	const tracked = path.join(repo, 'tests', 'fixtures', 'bench', 'target');
	fs.mkdirSync(path.join(tracked, 'debug'), { recursive: true });
	fs.writeFileSync(path.join(tracked, 'CACHEDIR.TAG'), `${CARGO_SIGNATURE}\n`);
	fs.writeFileSync(path.join(tracked, 'debug', 'fixture.json'), '{"fixture":true}');
	const git = (...args) => execFileSync('git', ['-C', repo, ...args], { stdio: 'pipe' });
	git('init', '-q');
	git('config', 'user.email', 'test@example.com');
	git('config', 'user.name', 'test');
	git('add', '-A', '-f');
	git('commit', '-qm', 'tracked fixture');
	aged(tracked, 30);

	const inventory = handleRequest({ schema: SCHEMA, operation: 'inventory' }, { env, now: Date.now() });
	const find = (directory) => inventory.items.find((entry) => entry.class === 'scratch' && entry.id.startsWith('cargo-target:') && entry.bytes >= 0 && entry.locator && entry._locator !== directory);
	const cargoItems = inventory.items.filter((entry) => entry.id.startsWith('cargo-target:'));

	// Exactly the genuine Cargo targets are inventoried: idle, building, recent.
	assert.equal(cargoItems.length, 3, `unsigned and tracked directories are not Cargo targets: ${JSON.stringify(cargoItems.map((entry) => entry.id))}`);
	assert.ok(find(idle), 'the idle target is inventoried');

	const reclaimable = cargoItems.filter((entry) => !entry.active);
	assert.equal(reclaimable.length, 1, 'only the idle target is offered for reclaim');
	assert.ok(reclaimable[0].reconstructable, 'Cargo output is reproducible');
	assert.ok(reclaimable[0].age_days >= 29, 'age is reported from the last write');

	const receipt = handleRequest(
		{ schema: SCHEMA, operation: 'reclaim', generation: inventory.generation, reclaim_targets: [{ id: reclaimable[0].id, reclaim_token: reclaimable[0].reclaim_token }] },
		{ env, now: Date.now() },
	);

	assert.deepEqual(receipt.reclaimed_item_ids, [reclaimable[0].id], 'the idle target is reclaimed');
	assert.ok(receipt.reclaimed_bytes > 0, 'reclaimed bytes are reported');
	assert.equal(fs.existsSync(idle), false, 'the idle target is removed');
	assert.equal(fs.existsSync(building), true, 'a live build is never disturbed');
	assert.equal(fs.existsSync(recent), true, 'recent output is never disturbed');
	assert.equal(fs.existsSync(unsigned), true, 'an unsigned directory is never treated as Cargo output');
	assert.equal(fs.existsSync(path.join(tracked, 'debug', 'fixture.json')), true, 'git-tracked fixtures survive');

	console.log('opencode external storage cargo targets: ok');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
