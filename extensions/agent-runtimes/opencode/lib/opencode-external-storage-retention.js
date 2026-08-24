'use strict';

const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const SCHEMA = 'homeboy/external-storage-retention/v1';
const PROVIDER_ID = 'opencode.external-storage-retention';
const CONFIG_ENV = 'HOMEBOY_OPENCODE_RETENTION_CONFIG';
const MARKER = '.homeboy-opencode-retention.json';
const MAX_REQUEST_BYTES = 256 * 1024;
const MAX_CONFIG_BYTES = 256 * 1024;
const MAX_TARGETS = 1000;
const MAX_ITEMS = 5000;
const MAX_ROOTS = 32;
const MAX_COMMAND_BYTES = 1024 * 1024;
const MAX_WALK_ENTRIES = 10_000;
const MAX_WALK_DEPTH = 32;
const MAX_WALK_BYTES = 1024 * 1024 * 1024 * 1024;
const CREDENTIAL_NAMES = new Set(['auth.json', 'account.json', 'mcp-auth.json', 'anthropic-oauth-accounts.json', 'openai-oauth-accounts.json']);

function externalStorageRetentionProviderContract() {
	return { id: PROVIDER_ID, command: ['homeboy-opencode-external-storage-retention'], timeout_seconds: 30 };
}

function handleRequest(request, options = {}) {
	validateRequest(request);
	const config = retentionConfig(options.env || process.env, options);
	const inventory = inventoryFor(config, options);
	if (request.operation === 'inventory') return inventory;
	if (request.generation !== inventory.generation) throw new Error('OpenCode storage inventory generation is stale.');
	const byId = new Map(inventory.items.map((item) => [item.id, item]));
	const reclaimed = [];
	let reclaimedBytes = 0;
	for (const target of request.reclaim_targets) {
		const item = byId.get(target.id);
		if (!item || item.reclaim_token !== target.reclaim_token || !reclaimable(item)) continue;
		const receipt = nativeReclaim(item, config, options);
		if (!receipt) continue;
		reclaimed.push(item.id);
		reclaimedBytes += receipt.bytes;
	}
	return { schema: SCHEMA, provider_id: PROVIDER_ID, generation: inventory.generation, reclaimed_item_ids: reclaimed, reclaimed_bytes: reclaimedBytes };
}

function writeOwnershipMarker(root, metadata = {}, env = process.env) {
	const key = markerKey(env);
	if (!key || !safeAbsoluteDirectory(root)) return false;
	const body = {
		schema: 'homeboy/opencode-retention-marker/v1', id: `scratch:${metadata.task_id}`, task_id: metadata.task_id,
		workspace: safeAbsolutePath(metadata.workspace), owner_pid: process.pid, active: true, created_at: new Date().toISOString(),
	};
	if (!validId(metadata.task_id)) return false;
	const marker = { ...body, signature: sign(body, key) };
	fs.writeFileSync(path.join(root, MARKER), JSON.stringify(marker), { mode: 0o600, flag: 'wx' });
	return true;
}

function finalizeOwnershipMarker(root, sessionId, env = process.env) {
	const marker = readMarker(root, env);
	if (!marker) return false;
	const body = { ...marker, owner_pid: 0, active: false, terminal_at: new Date().toISOString(), ...(validId(sessionId) ? { session_id: sessionId } : {}) };
	delete body.signature;
	fs.writeFileSync(path.join(root, MARKER), JSON.stringify({ ...body, signature: sign(body, markerKey(env)) }), { mode: 0o600 });
	return true;
}

function retentionConfig(env, options) {
	const value = readConfig(env[CONFIG_ENV]);
	const command = typeof value.command === 'string' && value.command.trim() ? value.command : options.command || 'opencode';
	const paths = openCodePaths(command, env);
	const dbPath = openCodeDbPath(command, env);
	const tempRoots = absolutePaths([...(value.temp_roots || []), paths.tmp].filter(Boolean));
	const dataRoots = absolutePaths([...(value.data_roots || []), paths.data, dbPath && path.dirname(dbPath)].filter(Boolean));
	if (tempRoots.length + dataRoots.length > MAX_ROOTS) throw new Error('Retention configuration exceeds the root ceiling.');
	if (tempRoots.some((root) => dataRoots.some((data) => overlaps(root, data)))) throw new Error('Retention roots must not overlap.');
	return { command, temp_roots: tempRoots, data_roots: dataRoots, db_path: dbPath, marker_key: markerKey(env) };
}

function inventoryFor(config, options) {
	const roots = [...config.temp_roots.map((value, index) => ({ id: `temp-${index}`, path: value })), ...config.data_roots.map((value, index) => ({ id: `data-${index}`, path: value }))];
	const sessions = discoverSessions(config, [], roots, options);
	const markerItems = config.marker_key ? discoverMarkers(config, roots, options, new Set(sessions.map((entry) => entry.id.slice('session:'.length)))) : [];
	const protectedItems = discoverProtected(config, roots);
	const compaction = pendingCompaction(config, options.env || process.env, roots);
	const items = [...markerItems, ...sessions, ...protectedItems, ...(compaction ? [compaction] : [])].slice(0, MAX_ITEMS);
	const known = new Set(items.map((item) => item._path).filter(Boolean));
	const unknownBytes = roots.reduce((total, root) => total + unknownBytesBelow(root.path, known), 0);
	const generation = digest(JSON.stringify({ roots: roots.map((root) => [root.id, fingerprint(root.path)]), items: items.map((item) => [item.id, item.state]) }));
	return {
		schema: SCHEMA, provider_id: PROVIDER_ID, generation, roots,
		items: items.map(({ _path, _workspace, state, ...item }) => ({ ...item, reclaim_token: digest(`${generation}:${item.id}:${state}`) })), unknown_bytes: unknownBytes,
	};
}

function discoverMarkers(config, roots, options, sessions) {
	const items = [];
	for (const root of config.temp_roots) {
		for (const candidate of [root, ...safeEntries(root).filter((entry) => entry.isDirectory()).slice(0, MAX_ITEMS).map((entry) => path.join(root, entry.name))]) {
			const marker = readMarker(candidate, options.env || process.env);
			if (!marker || !sameRealDirectory(candidate, root)) continue;
			const active = marker.active === true || processAlive(marker.owner_pid);
			const discovered = item(marker.id, rootId(candidate, roots), 'scratch', candidate, true, active, Boolean(marker.session_id && sessions.has(marker.session_id)), ageDays(marker.terminal_at, options.now), `marker:${marker.signature}`);
			discovered._workspace = marker.workspace;
			items.push(discovered);
		}
	}
	return items;
}

function discoverSessions(config, markerItems, roots, options) {
	if (!config.db_path || !safeRegularFile(config.db_path)) return [];
	const sessions = openCodeJson(config.command, ['session', 'list', '--format', 'json'], options.env || process.env);
	if (!Array.isArray(sessions)) return [item(`session-store:${digest(config.db_path).slice(0, 16)}`, rootId(config.db_path, roots), 'session_store', config.db_path, false, true, true, 0, `db:${fingerprint(config.db_path)}`)];
	const storeId = `session-store:${digest(config.db_path).slice(0, 16)}`;
	const markerSessions = markerSessionIds(config, options.env || process.env);
	const sessionItems = sessions.filter((session) => validId(session.id)).slice(0, MAX_ITEMS - 1).map((session) => item(`session:${session.id}`, rootId(config.db_path, roots), 'durable_artifact', config.db_path, true, processAlive(session.owner_pid), session.pinned === true || markerSessions.has(session.id), ageDays(session.updated || session.created, options.now), `session:${session.id}:${fingerprint(config.db_path)}`, 0));
	return [item(storeId, rootId(config.db_path, roots), 'session_store', config.db_path, false, false, true, 0, `db:${fingerprint(config.db_path)}`), ...sessionItems];
}
function markerSessionIds(config, env) { const ids = new Set(); for (const root of config.temp_roots) for (const candidate of [root, ...safeEntries(root).filter((entry) => entry.isDirectory()).slice(0, MAX_ITEMS).map((entry) => path.join(root, entry.name))]) { const marker = readMarker(candidate, env); if (marker?.session_id) ids.add(marker.session_id); } return ids; }

function discoverProtected(config, roots) {
	const items = [];
	for (const root of config.data_roots) {
		for (const entry of safeEntries(root).slice(0, MAX_ITEMS)) {
			const candidate = path.join(root, entry.name);
			if (CREDENTIAL_NAMES.has(entry.name)) items.push(item(`credential:${entry.name}`, rootId(candidate, roots), 'credential', candidate, false, false, true, 0, fingerprint(candidate)));
			if (/^(?:snapshot|export)/i.test(entry.name)) items.push(item(`pinned:${entry.name}`, rootId(candidate, roots), 'pinned_export', candidate, false, false, true, 0, fingerprint(candidate)));
		}
	}
	return items;
}

function nativeReclaim(itemToReclaim, config, options) {
	if (itemToReclaim.class === 'scratch') return reclaimScratch(itemToReclaim.id, config, options);
	if (itemToReclaim.id.startsWith('compaction:')) return retryCompaction(config, options.env || process.env);
	if (!itemToReclaim.id.startsWith('session:')) return null;
	const sessionId = itemToReclaim.id.slice('session:'.length);
	const before = sizeOf(config.db_path);
	const deleted = run(config.command, ['session', 'delete', sessionId], options.env || process.env);
	if (deleted.status !== 0) return null;
	const compacted = run(config.command, ['db', 'VACUUM'], options.env || process.env);
	// A successful deletion is a confirmed, idempotent mutation even if compaction fails.
	if (compacted.status !== 0) { writeCompaction(config, options.env || process.env); return { bytes: 0 }; }
	clearCompaction(options.env || process.env);
	return { bytes: Math.max(0, before - sizeOf(config.db_path)) };
}

function reclaimScratch(id, config, options) {
	const target = findMarkedScratch(id, config, options.env || process.env);
	if (!target) return null;
	if (!config.temp_roots.some((root) => sameRealDirectory(target, root)) || !readMarker(target, options.env || process.env)) return null;
	const bytes = sizeOf(target);
	const parent = path.dirname(target);
	const quarantine = path.join(parent, `.${path.basename(target)}.homeboy-reclaim-${crypto.randomUUID()}`);
	try {
		fs.renameSync(target, quarantine);
		if (!readMarker(quarantine, options.env || process.env)) { fs.renameSync(quarantine, target); return null; }
		fs.rmSync(quarantine, { recursive: true, force: false });
		return { bytes };
	} catch { return null; }
}

function findMarkedScratch(id, config, env) {
	for (const root of config.temp_roots) {
		for (const candidate of [root, ...safeEntries(root).filter((entry) => entry.isDirectory()).slice(0, MAX_ITEMS).map((entry) => path.join(root, entry.name))]) {
			if (sameRealDirectory(candidate, root) && readMarker(candidate, env)?.id === id) return candidate;
		}
	}
	return '';
}

function item(id, root, resourceClass, resourcePath, reconstructable, active, referenced, age, state, bytes) {
	return { id, root_id: root || 'unmanaged', class: resourceClass, bytes: bytes === undefined ? sizeOf(resourcePath) : bytes, locator: `opencode:${resourceClass}:${digest(resourcePath).slice(0, 16)}`, reconstructable, active, referenced, ownership_known: true, age_days: age, _path: resourcePath, state };
}
function readMarker(root, env) {
	const key = markerKey(env); const markerPath = path.join(root, MARKER);
	try {
		if (!key || fs.lstatSync(root).isSymbolicLink() || fs.lstatSync(markerPath).isSymbolicLink()) return null;
		const marker = JSON.parse(fs.readFileSync(markerPath, 'utf8')); const { signature, ...body } = marker;
		return marker.schema === 'homeboy/opencode-retention-marker/v1' && validId(marker.id) && validId(marker.task_id) && typeof signature === 'string' && secureEqual(signature, sign(body, key)) ? marker : null;
	} catch { return null; }
}
function validateRequest(request) {
	if (!request || typeof request !== 'object' || Array.isArray(request) || Buffer.byteLength(JSON.stringify(request)) > MAX_REQUEST_BYTES) throw new Error('Retention request is invalid or exceeds the protocol byte ceiling.');
	if (Object.keys(request).some((key) => !['schema', 'operation', 'generation', 'reclaim_targets'].includes(key)) || request.schema !== SCHEMA || !['inventory', 'reclaim'].includes(request.operation)) throw new Error('Expected a valid homeboy/external-storage-retention/v1 request.');
	if (request.operation === 'inventory' && (request.generation !== undefined || (request.reclaim_targets && request.reclaim_targets.length))) throw new Error('Inventory requests must not contain reclaim fields.');
	if (request.operation === 'reclaim' && (!validId(request.generation) || !Array.isArray(request.reclaim_targets) || request.reclaim_targets.length > MAX_TARGETS || request.reclaim_targets.some((target) => !target || Object.keys(target).some((key) => !['id', 'reclaim_token'].includes(key)) || !validId(target.id) || !validId(target.reclaim_token)))) throw new Error('Reclaim request is invalid or exceeds the protocol target ceiling.');
}
function readConfig(file) { if (!file) return {}; if (!safeRegularFile(file) || fs.statSync(file).size > MAX_CONFIG_BYTES) throw new Error('Retention configuration is invalid or exceeds its byte ceiling.'); const value = JSON.parse(fs.readFileSync(file, 'utf8')); if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some((key) => !['command', 'temp_roots', 'data_roots'].includes(key)) || (value.temp_roots && !Array.isArray(value.temp_roots)) || (value.data_roots && !Array.isArray(value.data_roots))) throw new Error('Retention configuration has an invalid shape.'); return value; }
function openCodePaths(command, env) { const result = run(command, ['debug', 'paths'], env); return result.status === 0 ? Object.fromEntries(String(result.stdout).split(/\r?\n/).map((line) => line.trim().split(/\s{2,}/)).filter(([key, value]) => key && value)) : {}; }
function openCodeDbPath(command, env) { const result = run(command, ['db', 'path'], env); const candidate = String(result.stdout || '').trim(); return result.status === 0 && safeAbsolutePath(candidate) ? candidate : ''; }
function openCodeJson(command, args, env) { const result = run(command, args, env); if (result.status !== 0 || Buffer.byteLength(result.stdout || '') > MAX_COMMAND_BYTES) return null; try { return JSON.parse(result.stdout); } catch { return null; } }
function run(command, args, env) { return spawnSync(command, args, { encoding: 'utf8', env, maxBuffer: MAX_COMMAND_BYTES }); }
function markerKey(env) {
	try {
		const state = env?.XDG_STATE_HOME || (env?.HOME && path.join(env.HOME, '.local', 'state'));
		const directory = safeStateDirectory(state); if (!directory) return '';
		const file = path.join(directory, 'opencode-retention.key');
		if (!fs.existsSync(file)) fs.writeFileSync(file, crypto.randomBytes(32).toString('hex'), { mode: 0o600, flag: 'wx' });
		const stat = fs.lstatSync(file); if (!stat.isFile() || stat.isSymbolicLink() || (stat.mode & 0o077) !== 0) return '';
		const key = fs.readFileSync(file, 'utf8').trim(); return /^[a-f0-9]{64}$/.test(key) ? key : '';
	} catch { return ''; }
}
function safeStateDirectory(state) {
	const absolute = safeAbsolutePath(state); if (!absolute) return '';
	let ancestor = absolute; const tail = [];
	while (!fs.existsSync(ancestor)) { tail.unshift(path.basename(ancestor)); ancestor = path.dirname(ancestor); }
	if (fs.lstatSync(ancestor).isSymbolicLink()) return '';
	let current = fs.realpathSync(ancestor);
	for (const part of tail) { current = path.join(current, part); if (fs.existsSync(current) && fs.lstatSync(current).isSymbolicLink()) return ''; fs.mkdirSync(current, { mode: 0o700 }); }
	const directory = path.join(current, 'homeboy'); if (fs.existsSync(directory) && fs.lstatSync(directory).isSymbolicLink()) return '';
	fs.mkdirSync(directory, { recursive: true, mode: 0o700 }); return fs.realpathSync(directory);
}
function stateFile(env, name) { const state = env?.XDG_STATE_HOME || (env?.HOME && path.join(env.HOME, '.local', 'state')); return safeAbsolutePath(state) ? path.join(state, 'homeboy', name) : ''; }
function writeCompaction(config, env) { try { const file = stateFile(env, 'opencode-retention-compaction.json'); if (!file) return; fs.writeFileSync(file, JSON.stringify({ db_path: config.db_path, created_at: new Date().toISOString() }), { mode: 0o600 }); } catch { /* The successful session receipt remains truthful even if retry state is unavailable. */ } }
function clearCompaction(env) { try { const file = stateFile(env, 'opencode-retention-compaction.json'); if (file) fs.unlinkSync(file); } catch { /* No pending state is equivalent to cleared state. */ } }
function pendingCompaction(config, env, roots) { try { const file = stateFile(env, 'opencode-retention-compaction.json'); const state = file && JSON.parse(fs.readFileSync(file, 'utf8')); return state?.db_path === config.db_path ? item(`compaction:${digest(config.db_path).slice(0, 16)}`, rootId(config.db_path, roots), 'session_store', config.db_path, true, false, false, ageDays(state.created_at), `compaction:${fingerprint(file)}`, 0) : null; } catch { return null; } }
function retryCompaction(config, env) { const before = sizeOf(config.db_path); const compacted = run(config.command, ['db', 'VACUUM'], env); if (compacted.status !== 0) return null; clearCompaction(env); return { bytes: Math.max(0, before - sizeOf(config.db_path)) }; }
function sign(value, key) { return crypto.createHmac('sha256', key).update(JSON.stringify(value)).digest('hex'); }
function secureEqual(left, right) { return left.length === right.length && crypto.timingSafeEqual(Buffer.from(left), Buffer.from(right)); }
function reclaimable(value) { return value.ownership_known && value.reconstructable && !value.active && !value.referenced && !['credential', 'pinned_export'].includes(value.class); }
function processAlive(pid) { if (!Number.isInteger(pid) || pid <= 0) return false; try { process.kill(pid, 0); return true; } catch (error) { return error?.code === 'EPERM'; } }
function safeEntries(directory, limit = MAX_WALK_ENTRIES) { try { const handle = fs.opendirSync(directory, { bufferSize: Math.min(limit, 128) }); const entries = []; for (let entry = handle.readSync(); entry && entries.length < limit; entry = handle.readSync()) entries.push(entry); handle.closeSync(); return entries; } catch { return []; } }
function sizeOf(candidate) {
	const stack = [[candidate, 0]]; let bytes = 0; let entries = 0;
	while (stack.length && entries < MAX_WALK_ENTRIES && bytes < MAX_WALK_BYTES) {
		const [current, depth] = stack.pop(); entries += 1;
		try { const stat = fs.lstatSync(current); if (stat.isSymbolicLink()) continue; if (stat.isDirectory()) { if (depth < MAX_WALK_DEPTH) for (const entry of safeEntries(current).slice(0, MAX_WALK_ENTRIES - entries)) stack.push([path.join(current, entry.name), depth + 1]); } else bytes += stat.size; } catch { /* Unknown paths remain non-reclaimable. */ }
	}
	return bytes;
}
function unknownBytesBelow(root, known) { const stack = [[root, 0]]; let bytes = 0; let entries = 0; while (stack.length && entries < MAX_WALK_ENTRIES && bytes < MAX_WALK_BYTES) { const [current, depth] = stack.pop(); entries += 1; if (known.has(current)) continue; const hasKnownDescendant = [...known].some((candidate) => inside(candidate, current)); if (!hasKnownDescendant) { bytes += sizeOf(current); continue; } if (depth < MAX_WALK_DEPTH) for (const entry of safeEntries(current, MAX_WALK_ENTRIES - entries)) stack.push([path.join(current, entry.name), depth + 1]); } return bytes; }
function rootId(candidate, roots) { return roots.find((root) => inside(candidate, root.path))?.id || 'unmanaged'; }
function sameRealDirectory(candidate, root) { try { return inside(fs.realpathSync(candidate), fs.realpathSync(root)) && !fs.lstatSync(candidate).isSymbolicLink(); } catch { return false; } }
function safeRegularFile(candidate) { try { return safeAbsolutePath(candidate) && fs.lstatSync(candidate).isFile() && !fs.lstatSync(candidate).isSymbolicLink(); } catch { return false; } }
function safeAbsoluteDirectory(candidate) { try { return safeAbsolutePath(candidate) && fs.lstatSync(candidate).isDirectory() && !fs.lstatSync(candidate).isSymbolicLink(); } catch { return false; } }
function safeAbsolutePath(value) { return typeof value === 'string' && path.isAbsolute(value) ? path.resolve(value) : ''; }
function absolutePaths(values) { return [...new Set(values.map(safeAbsolutePath).filter(Boolean))]; }
function inside(candidate, root) { if (!candidate || !root) return false; const relative = path.relative(root, candidate); return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative)); }
function overlaps(one, two) { return inside(one, two) || inside(two, one); }
function fingerprint(candidate) { try { const stat = fs.lstatSync(candidate); return `${stat.dev}:${stat.ino}:${stat.size}:${stat.mtimeMs}`; } catch { return 'missing'; } }
function digest(value) { return crypto.createHash('sha256').update(value).digest('hex'); }
function validId(value) { return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value); }
function ageDays(value, now = Date.now()) { const time = typeof value === 'number' ? value : Date.parse(value); return Number.isFinite(time) && time <= now ? Math.floor((now - time) / 86_400_000) : 0; }

module.exports = { CONFIG_ENV, MARKER, PROVIDER_ID, SCHEMA, externalStorageRetentionProviderContract, finalizeOwnershipMarker, handleRequest, retentionConfig, writeOwnershipMarker };
