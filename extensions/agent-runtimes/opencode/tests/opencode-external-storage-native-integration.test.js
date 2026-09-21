'use strict';

// This is a real dependency test. It uses a disposable database and an
// ephemeral Bun wrapper, never the user's OpenCode database.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { CONFIG_ENV, SCHEMA, handleRequest } = require('../lib/opencode-external-storage-retention');

const nativeRoot = process.env.HOMEBOY_OPENCODE_NATIVE_ROOT;
assert.ok(nativeRoot, 'Set HOMEBOY_OPENCODE_NATIVE_ROOT to the compatible native OpenCode checkout');
assert.equal(fs.existsSync(path.join(nativeRoot, 'packages/opencode/src/index.ts')), true, `native source missing: ${nativeRoot}`);
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opencode-native-'));
const home = path.join(root, 'home');
const data = path.join(home, '.local', 'share');
const state = path.join(home, '.local', 'state');
const database = path.join(data, 'opencode', 'opencode.db');
const backup = path.join(root, 'verified-backup.db');
const command = path.join(root, 'opencode-native-wrapper');
const nativeScript = path.join(nativeRoot, 'packages/core', `.homeboy-native-integration-${process.pid}.ts`);
for (const directory of [data, state, path.join(home, '.config'), path.join(home, '.cache')]) fs.mkdirSync(directory, { recursive: true });
fs.writeFileSync(command, `#!/bin/sh\nexec bun ${JSON.stringify(path.join(nativeRoot, 'packages/opencode/src/index.ts'))} "$@"\n`, { mode: 0o700 });

const env = {
	...process.env,
	HOME: home,
	OPENCODE_TEST_HOME: home,
	OPENCODE_DB: database,
	XDG_CONFIG_HOME: path.join(home, '.config'),
	XDG_DATA_HOME: data,
	XDG_STATE_HOME: state,
	XDG_CACHE_HOME: path.join(home, '.cache'),
	OPENCODE_DISABLE_PROJECT_CONFIG: '1',
	OPENCODE_PURE: '1',
	OPENCODE_DISABLE_AUTOUPDATE: '1',
	OPENCODE_DISABLE_MODELS_FETCH: '1',
};
const config = path.join(root, 'retention.json');
fs.writeFileSync(config, JSON.stringify({ command, temp_roots: [], data_roots: [], operation_timeout_ms: 120000 }));
env[CONFIG_ENV] = config;

function nativeCore(code) {
	fs.writeFileSync(nativeScript, code, { mode: 0o600 });
	const result = spawnSync('bun', ['run', nativeScript], { cwd: nativeRoot, env, encoding: 'utf8', timeout: 120000, maxBuffer: 4 * 1024 * 1024 });
	fs.rmSync(nativeScript, { force: true });
	assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`);
	return JSON.parse(result.stdout.trim());
}

try {
	const fixture = nativeCore(`
import { Context, Effect, Layer } from "effect"
import { eq } from "drizzle-orm"
import { AppNodeBuilder } from "@opencode-ai/core/effect/app-node-builder"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { Database } from "@opencode-ai/core/database/database"
import { EventV2 } from "@opencode-ai/core/event"
import { EventTable } from "@opencode-ai/core/event/sql"
import { Project } from "@opencode-ai/core/project"
import { ProjectTable } from "@opencode-ai/core/project/sql"
import { ProviderV2 } from "@opencode-ai/core/provider"
import { ModelV2 } from "@opencode-ai/core/model"
import { SessionProjector } from "@opencode-ai/core/session/projector"
import { MessageTable, SessionTable } from "@opencode-ai/core/session/sql"
import { SessionV1 } from "@opencode-ai/schema/session-v1"
import { SessionID } from "@opencode-ai/schema/session-id"
import { AbsolutePath } from "@opencode-ai/core/schema"
const layer = AppNodeBuilder.build(LayerNode.group([Database.node, EventV2.node, SessionProjector.node]))
const result = await Effect.runPromise(Effect.scoped(Effect.gen(function* () {
  const { db } = yield* Database.Service
  const events = yield* EventV2.Service
  const sessionID = SessionID.descending("ses_homeboy_native_fixture")
  const messageID = SessionV1.MessageID.ascending("msg_homeboy_native_fixture")
  yield* db.insert(ProjectTable).values({ id: Project.ID.global, worktree: AbsolutePath.make("/disposable"), sandboxes: [] }).run()
  yield* db.insert(SessionTable).values({ id: sessionID, project_id: Project.ID.global, slug: "native-fixture", directory: "/disposable", title: "native-fixture", version: "fixture" }).run()
  const message = (agent) => ({ id: messageID, sessionID, role: "user", time: { created: 1 }, agent, model: { providerID: ProviderV2.ID.make("provider"), modelID: ModelV2.ID.make("model") } })
  yield* events.publish(SessionV1.Event.MessageUpdated, { sessionID, info: message("before") })
  yield* events.publish(SessionV1.Event.MessageUpdated, { sessionID, info: message("after") })
  const rows = yield* db.select().from(EventTable).where(eq(EventTable.aggregate_id, sessionID)).all()
  const projection = yield* db.select().from(MessageTable).where(eq(MessageTable.id, messageID)).get()
  return { sessionID, messageID, events: rows.length, projection: projection.data }
}).pipe(Effect.provide(layer))))
console.log(JSON.stringify(result))
`);

	const before = nativeCore(`
import { Effect } from "effect"
import { AppNodeBuilder } from "@opencode-ai/core/effect/app-node-builder"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { Database } from "@opencode-ai/core/database/database"
import { EventV2 } from "@opencode-ai/core/event"
import { EventTable } from "@opencode-ai/core/event/sql"
import { MessageTable, SessionTable } from "@opencode-ai/core/session/sql"
import { SessionProjector } from "@opencode-ai/core/session/projector"
const layer = AppNodeBuilder.build(LayerNode.group([Database.node, EventV2.node, SessionProjector.node]))
const result = await Effect.runPromise(Effect.scoped(Effect.gen(function* () { const { db } = yield* Database.Service; const events = yield* EventV2.Service; const rows = yield* db.select().from(EventTable).all(); const message = yield* db.select().from(MessageTable).get(); const sessions = yield* db.select().from(SessionTable).all(); const replay = yield* events.replayAll(rows.map((event) => ({ id: event.id, aggregateID: event.aggregate_id, seq: event.seq, type: event.type, data: event.data }))); return { events: rows.length, message: message.data, sessions: sessions.length, replay } }).pipe(Effect.provide(layer))))
console.log(JSON.stringify(result))
`);
	assert.equal(before.events, 2);
	assert.equal(before.sessions, 1);
	assert.equal(before.message.agent, 'after');
	assert.equal(before.replay, fixture.sessionID);

	const inventory = handleRequest({ schema: SCHEMA, operation: 'inventory' }, { env });
	const compaction = inventory.items.find((item) => item.id.startsWith('compaction:'));
	assert.ok(compaction, 'native status must expose pending compaction');
	const receipt = handleRequest({ schema: SCHEMA, operation: 'reclaim', generation: inventory.generation, reclaim_targets: [{ id: compaction.id, reclaim_token: compaction.reclaim_token }] }, { env });
	assert.deepEqual(Object.keys(receipt).sort(), ['generation', 'provider_id', 'reclaimed_bytes', 'reclaimed_item_ids', 'schema']);
	assert.deepEqual(receipt.reclaimed_item_ids, [compaction.id]);
	assert.equal(receipt.reclaimed_bytes, 0, 'bounded rewrite has no physical reclaim');

	const after = nativeCore(`
import { Effect } from "effect"
import { AppNodeBuilder } from "@opencode-ai/core/effect/app-node-builder"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { Database } from "@opencode-ai/core/database/database"
import { EventV2 } from "@opencode-ai/core/event"
import { EventTable } from "@opencode-ai/core/event/sql"
import { MessageTable, SessionTable } from "@opencode-ai/core/session/sql"
import { SessionEventLogCompaction } from "@opencode-ai/core/session/event-log-compaction"
import { SessionProjector } from "@opencode-ai/core/session/projector"
const layer = AppNodeBuilder.build(LayerNode.group([Database.node, EventV2.node, SessionProjector.node]))
const result = await Effect.runPromise(Effect.scoped(Effect.gen(function* () { const { db } = yield* Database.Service; const events = yield* EventV2.Service; const rows = yield* db.select().from(EventTable).all(); const message = yield* db.select().from(MessageTable).get(); const sessions = yield* db.select().from(SessionTable).all(); const replay = yield* events.replayAll(rows.map((event) => ({ id: event.id, aggregateID: event.aggregate_id, seq: event.seq, type: event.type, data: event.data }))); const status = yield* SessionEventLogCompaction.status(db); return { events: rows.length, types: rows.map((event) => event.type), message: message.data, sessions: sessions.length, replay, status } }).pipe(Effect.provide(layer))))
console.log(JSON.stringify(result))
`);
	assert.equal(after.events, before.events);
	assert.equal(after.sessions, before.sessions);
	assert.equal(after.message.agent, before.message.agent);
	assert.equal(after.replay, fixture.sessionID);
	assert.equal(after.types.includes('event.compacted.1'), true);
	assert.equal(after.status.compactableEvents, 1);

	const maintenance = spawnSync(command, ['db', 'compact-events', '--all', '--apply', '--until-done', '--vacuum', '--backup', backup, '--limit', '1000'], { env, encoding: 'utf8', timeout: 120000, maxBuffer: 4 * 1024 * 1024 });
	assert.equal(maintenance.status, 0, `${maintenance.stderr}\n${maintenance.stdout}`);
	const maintenanceResult = JSON.parse(maintenance.stdout);
	assert.equal(maintenanceResult.contract, 'opencode.db.compact-events.v1');
	assert.equal(maintenanceResult.capabilities.physicalReclamation, 'supported-with-verified-backup');
	assert.equal(Number.isSafeInteger(maintenanceResult.bytes.logicalPayloadReclaimed), true);
	assert.equal(Number.isSafeInteger(maintenanceResult.bytes.physicalReclaimed), true);
	assert.equal(fs.existsSync(backup), true, 'native verified backup exists');
	const backupCheck = spawnSync('sqlite3', [backup, 'PRAGMA integrity_check; SELECT count(*) FROM event;'], { encoding: 'utf8' });
	assert.equal(backupCheck.status, 0, backupCheck.stderr);
	assert.match(backupCheck.stdout, /ok\n2\s*$/);
	console.log(`opencode native integration: ok (${nativeRoot}@${spawnSync('git', ['rev-parse', 'HEAD'], { cwd: nativeRoot, encoding: 'utf8' }).stdout.trim()})`);
} finally {
	fs.rmSync(nativeScript, { force: true });
	fs.rmSync(root, { recursive: true, force: true });
}
