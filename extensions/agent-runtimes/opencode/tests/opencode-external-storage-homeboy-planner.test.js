'use strict';

// Exercises the real Homeboy external-storage planner in an isolated HOME.
// The SQLite file is disposable; no user database or live provider is used.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const homeboyRoot = process.env.HOMEBOY_CORE_ROOT || '/Users/chubes/Developer/homeboy';
const homeboy = process.env.HOMEBOY_BIN || path.join(homeboyRoot, 'target', 'debug', 'homeboy');
assert.equal(fs.existsSync(homeboy), true, `Homeboy binary missing: ${homeboy}`);
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-planner-'));
const home = path.join(root, 'home');
const configRoot = path.join(home, '.config', 'homeboy');
const extensionRoot = path.join(configRoot, 'extensions', 'fixture-provider');
const database = path.join(root, 'native.sqlite');
const provider = path.join(root, 'fixture-provider.cjs');
const fixtureBytes = 4096;
for (const directory of [extensionRoot, path.dirname(database)]) fs.mkdirSync(directory, { recursive: true });

function runSql(sql) {
	const result = spawnSync('sqlite3', [database, sql], { encoding: 'utf8' });
	assert.equal(result.status, 0, result.stderr);
}

try {
	runSql(`CREATE TABLE fixture_payload (id TEXT PRIMARY KEY, data BLOB); INSERT INTO fixture_payload VALUES ('retention-fixture', zeroblob(${fixtureBytes}));`);
	fs.writeFileSync(provider, `#!/usr/bin/env node
const fs = require('node:fs');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
const db = process.env.FIXTURE_DB;
const stat = fs.statSync(db);
const item = {id:'fixture:sqlite',root_id:'native-db',class:'durable_artifact',bytes:stat.size,locator:'fixture:sqlite',reconstructable:true,active:false,referenced:false,ownership_known:true,age_days:Math.floor((Date.now()-stat.mtimeMs)/86400000),reclaim_token:'fixture-token'};
if (input.operation === 'inventory') process.stdout.write(JSON.stringify({schema:'homeboy/external-storage-retention/v1',provider_id:'fixture-provider',generation:'fixture-generation',roots:[{id:'native-db',path:db}],items:[item],unknown_bytes:0}));
else process.stdout.write(JSON.stringify({schema:'homeboy/external-storage-retention/v1',provider_id:'fixture-provider',generation:input.generation,reclaimed_item_ids:input.reclaim_targets.map((target) => target.id),reclaimed_bytes:0}));
`, { mode: 0o700 });
fs.writeFileSync(path.join(extensionRoot, 'fixture-provider.json'), JSON.stringify({ name: 'Fixture provider', version: '1.0.0', external_storage_retention: { providers: [{ id: 'fixture-provider', command: ['node', provider], timeout_seconds: 10 }] } }));
const env = { ...process.env, HOME: home, HOMEBOY_DATA_DIR: path.join(home, 'data'), FIXTURE_DB: database, XDG_CONFIG_HOME: path.join(home, '.config'), XDG_DATA_HOME: path.join(home, '.local', 'share'), XDG_STATE_HOME: path.join(home, '.local', 'state') };
const command = (config, apply = false) => {
	const configPath = path.join(configRoot, 'homeboy.json');
	if (config) fs.writeFileSync(configPath, JSON.stringify(config));
	else fs.rmSync(configPath, { force: true });
	const result = spawnSync(homeboy, ['--placement', 'local', 'cleanup', '--include', 'external-storage', ...(apply ? ['--apply'] : [])], { env, encoding: 'utf8', timeout: 120000 });
	assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`);
	return JSON.parse(result.stdout).data;
};

	const defaults = command(undefined);
	assert.equal(defaults.candidate_count, 0, 'default seven-day policy must not select age-zero fixture');
	assert.equal(defaults.applied_count, 0);

	const policy = { retention: { external_storage_days: 0, external_storage_max_bytes: 20 * 1024 * 1024 * 1024 } };
	const override = command(policy);
	assert.equal(override.candidate_count, 1, 'explicit supported age override selects measured fixture');
	assert.equal(override.estimated_bytes, fs.statSync(database).size, 'planner uses measured provider bytes');
	assert.equal(override.categories[0].output.providers[0].candidates[0].bytes, fs.statSync(database).size);
	const applied = command(policy, true);
	assert.equal(applied.applied_count, 1, 'real Homeboy cleanup applies the selected fixture provider');

	console.log(`homeboy planner integration: ok (fixture=${database}, measured_bytes=${fs.statSync(database).size}, default_candidates=${defaults.candidate_count}, override_candidates=${override.candidate_count})`);
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
