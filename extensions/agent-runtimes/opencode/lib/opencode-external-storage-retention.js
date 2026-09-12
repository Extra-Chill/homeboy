'use strict';

const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const SCHEMA = 'homeboy/external-storage-retention/v1';
const PROVIDER_ID = 'opencode.external-storage-retention';
const CONFIG_ENV = 'HOMEBOY_OPENCODE_RETENTION_CONFIG';
const MARKER = '.homeboy-opencode-retention.json';
const CARGO_TARGET_PREFIX = 'cargo-target:';
// The signature Cargo writes into CACHEDIR.TAG in every target directory.
const CACHEDIR_TAG_SIGNATURE = 'Signature: 8a477f597d28d172789f06886806bc55';
const CARGO_ACTIVE_WINDOW_MS = 60 * 60 * 1000;
const MAX_CARGO_WALK_DEPTH = 6;
const CARGO_WALK_SKIP = new Set(['.git', 'node_modules', 'vendor']);
const GIT_PROBE_TIMEOUT_MS = 10_000;
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
const HISTORY_NAMES = new Set(['history', 'history.json', 'history.db', 'messages', 'messages.json']);

function externalStorageRetentionProviderContract() {
	return { id: PROVIDER_ID, command: ['node', '{{extension_path}}/scripts/agent/homeboy-opencode-external-storage-retention.cjs'], timeout_seconds: 30 };
}

function handleRequest(request, options = {}) {
	validateRequest(request);
	const config = retentionConfig(options.env || process.env, options);
	const inventory = inventoryFor(config, options);
	if (request.operation === 'inventory') return inventory;
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
	// Echo the generation the reclaim was requested against. Homeboy validates
	// this to confirm the provider acted on the inventory view it was handed;
	// returning a freshly recomputed generation fails that check on any root
	// that saw unrelated writes since inventory (#2832). Per-item reclaim
	// tokens, not this value, decide whether an individual item may be removed.
	return { schema: SCHEMA, provider_id: PROVIDER_ID, generation: request.generation, reclaimed_item_ids: reclaimed, reclaimed_bytes: reclaimedBytes };
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
	if (hasOverlappingRoots(tempRoots) || hasOverlappingRoots(dataRoots)) throw new Error('Retention roots in the same storage class must not overlap.');
	if (tempRoots.some((root) => dataRoots.some((data) => overlaps(root, data)))) throw new Error('Retention roots must not overlap.');
	return { command, temp_roots: tempRoots, data_roots: dataRoots, db_path: dbPath, marker_key: markerKey(env) };
}

function inventoryFor(config, options) {
	const roots = [...config.temp_roots.map((value, index) => ({ id: `temp-${index}`, path: value })), ...config.data_roots.map((value, index) => ({ id: `data-${index}`, path: value }))];
	const markerDiscovery = config.marker_key ? discoverMarkers(config, roots, options, new Set()) : { items: [], incomplete: [] };
	const sessionDiscovery = discoverSessions(config, roots, options, new Set(markerDiscovery.items.map((entry) => entry._session_id).filter(Boolean)));
	for (const marker of markerDiscovery.items) marker.referenced = Boolean(marker._session_id && sessionDiscovery.ids.has(marker._session_id));
	const protectedItems = discoverProtected(config, roots);
	const cargoDiscovery = discoverCargoTargets(config, options, new Set([...markerDiscovery.items, ...sessionDiscovery.items].map((entry) => entry._path).filter(Boolean)));
	// Scratch is discovered before session rows so a busy session store cannot
	// starve signed terminal scratch from the bounded inventory.
	const items = [...markerDiscovery.items, ...sessionDiscovery.items, ...protectedItems, ...cargoDiscovery.items].slice(0, MAX_ITEMS);
	const known = new Set(items.map((item) => item._path).filter(Boolean));
	const unknown = roots.map((root) => unknownBytesBelow(root.path, known));
	const unknownBytes = unknown.reduce((total, value) => total + value.bytes, 0);
	const measuredItems = items.map((entry) => ({ entry, measurement: walkBytes(entry._path) }));
	const incomplete = [...markerDiscovery.incomplete, ...cargoDiscovery.incomplete, ...unknown.flatMap((value, index) => value.reasons.map((reason) => ({ root_id: roots[index].id, reason, observed_entries: value.entries, observed_bytes: value.bytes })) ), ...measuredItems.flatMap(({ entry, measurement }) => measurement.reasons.map((reason) => ({ root_id: entry.root_id, reason: `item_${reason}`, observed_entries: measurement.entries, observed_bytes: measurement.bytes })) )];
	const generation = digest(JSON.stringify({ roots: roots.map((root) => [root.id, fingerprint(root.path)]), items: items.map((item) => [item.id, item.state]) }));
	return {
		schema: SCHEMA, provider_id: PROVIDER_ID, generation, roots,
		items: items.map(({ _path, _workspace, _session_id, state, ...item }) => ({ ...item, reclaim_token: reclaimToken(item.id, state, _path) })), unknown_bytes: unknownBytes,
		...(incomplete.length ? { completeness: { complete: false, incomplete_roots: incomplete } } : {}),
	};
}

function discoverMarkers(config, roots, options, sessions) {
	const items = []; const incomplete = [];
	for (const root of config.temp_roots) {
		const discovery = boundedDirectories(root);
		for (const candidate of discovery.directories) {
			const marker = readMarker(candidate, options.env || process.env);
			if (!marker || !sameRealDirectory(candidate, root)) continue;
			const active = marker.active === true || processAlive(marker.owner_pid);
			const discovered = item(marker.id, rootId(candidate, roots), 'scratch', candidate, true, active, Boolean(marker.session_id && sessions.has(marker.session_id)), ageDays(marker.terminal_at, options.now), `marker:${marker.signature}:${sizeOf(candidate)}`);
			discovered._workspace = marker.workspace;
			discovered._session_id = marker.session_id;
			items.push(discovered);
		}
		for (const reason of discovery.reasons) incomplete.push({ root_id: rootId(root, roots), reason: `scratch_discovery_${reason}`, observed_entries: discovery.entries, observed_bytes: 0 });
	}
	return { items, incomplete };
}

function discoverSessions(config, roots, options, markerSessions = markerSessionIds(config, options.env || process.env)) {
	if (!config.db_path || !safeRegularFile(config.db_path)) return { items: [], ids: new Set() };
	const sessions = openCodeJson(config.command, ['session', 'list', '--format', 'json'], options.env || process.env);
	if (!Array.isArray(sessions)) return { items: [item(`session-store:${digest(config.db_path).slice(0, 16)}`, rootId(config.db_path, roots), 'session_store', config.db_path, false, true, true, 0, `db:${fingerprint(config.db_path)}`)], ids: new Set() };
	const storeId = `session-store:${digest(config.db_path).slice(0, 16)}`;
	const validSessions = sessions.filter((session) => validId(session.id));
	const sessionItems = validSessions.slice(0, MAX_ITEMS - 1).map((session) => item(`session:${session.id}`, rootId(config.db_path, roots), 'durable_artifact', config.db_path, true, processAlive(session.owner_pid), session.pinned === true || markerSessions.has(session.id), ageDays(session.updated || session.created, options.now), `session:${session.id}:${fingerprint(config.db_path)}`, 0));
	return { items: [item(storeId, rootId(config.db_path, roots), 'session_store', config.db_path, false, false, true, 0, `db:${fingerprint(config.db_path)}`), ...sessionItems], ids: new Set(validSessions.map((session) => session.id)) };
}
function markerSessionIds(config, env) { const ids = new Set(); for (const root of config.temp_roots) for (const candidate of boundedDirectories(root).directories) { const marker = readMarker(candidate, env); if (marker?.session_id) ids.add(marker.session_id); } return ids; }

function discoverProtected(config, roots) {
	const items = [];
	for (const root of config.data_roots) {
		for (const entry of safeEntries(root).slice(0, MAX_ITEMS)) {
			const candidate = path.join(root, entry.name);
			if (CREDENTIAL_NAMES.has(entry.name)) items.push(item(`credential:${entry.name}`, rootId(candidate, roots), 'credential', candidate, false, false, true, 0, fingerprint(candidate)));
			if (HISTORY_NAMES.has(entry.name)) items.push(item(`history:${entry.name}`, rootId(candidate, roots), 'history', candidate, false, false, true, 0, fingerprint(candidate)));
			if (/^(?:snapshot|export)/i.test(entry.name)) items.push(item(`pinned:${entry.name}`, rootId(candidate, roots), 'pinned_export', candidate, false, false, true, 0, fingerprint(candidate)));
		}
	}
	return items;
}

function nativeReclaim(itemToReclaim, config, options) {
	if (itemToReclaim.id.startsWith(CARGO_TARGET_PREFIX)) return reclaimCargoTarget(itemToReclaim.id, config, options);
	if (itemToReclaim.class === 'scratch') return reclaimScratch(itemToReclaim.id, config, options);
	return null;
}

// Cargo target directories accumulate under the temp root faster than any
// manual sweep clears them, and nothing owned them: an explicitly chosen
// target is caller-owned to Homeboy core, so it is never reclaimed there
// (#2838). A target that lives inside a root this provider already owns is
// different — it is reproducible by definition, and Cargo stamps every target
// with its own CACHEDIR.TAG signature, so it can be identified by content
// rather than by the directory name.
// Cargo targets sit shallow, while a temp root holds deep, enormous trees.
// A depth-first walk spends its whole entry budget inside the first subtree it
// enters and never reaches them, so search breadth-first, never descend into a
// target once identified, and skip trees that cannot contain build output.
function cargoTargetDirectories(root) {
	const queue = [[root, 0]]; const targets = []; let entries = 0; const reasons = new Set();
	while (queue.length) {
		if (entries >= MAX_WALK_ENTRIES) { reasons.add('entry_limit'); break; }
		const [current, depth] = queue.shift(); entries += 1;
		let stat;
		try { stat = fs.lstatSync(current); } catch { reasons.add('read_error'); continue; }
		if (stat.isSymbolicLink() || !stat.isDirectory()) continue;
		if (isCargoTargetDirectory(current)) { targets.push(current); continue; }
		if (depth >= MAX_CARGO_WALK_DEPTH) { reasons.add('depth_limit'); continue; }
		for (const entry of safeEntries(current, MAX_WALK_ENTRIES - entries)) {
			if (entry.isDirectory() && !CARGO_WALK_SKIP.has(entry.name)) queue.push([path.join(current, entry.name), depth + 1]);
		}
	}
	return { targets, entries, reasons: [...reasons] };
}

function discoverCargoTargets(config, options, known) {
	const items = []; const incomplete = [];
	const now = options.now || Date.now();
	for (const root of config.temp_roots) {
		const discovery = cargoTargetDirectories(root);
		for (const reason of discovery.reasons) incomplete.push({ root_id: rootIdFor(root, config), reason: `cargo_${reason}`, observed_entries: discovery.entries, observed_bytes: 0 });
		for (const candidate of discovery.targets) {
			if (known.has(candidate)) continue;
			if (!containedInRoot(candidate, root)) continue;
			if (containsTrackedFiles(candidate)) continue;
			const id = `${CARGO_TARGET_PREFIX}${digest(candidate).slice(0, 32)}`;
			const measurement = walkBytes(candidate);
			for (const reason of measurement.reasons) incomplete.push({ root_id: rootIdFor(root, config), reason: `cargo_target_${reason}`, observed_entries: measurement.entries, observed_bytes: measurement.bytes });
			items.push(item(id, rootIdFor(root, config), 'scratch', candidate, true, cargoTargetActive(candidate, now), false, cargoTargetAgeDays(candidate, now), fingerprint(candidate), measurement.bytes));
		}
	}
	return { items, incomplete };
}

// Cargo writes this exact signature into `CACHEDIR.TAG` in every target
// directory. Matching on it, rather than on a directory named `target`, is
// what keeps a tracked fixture that merely looks like build output safe.
function isCargoTargetDirectory(candidate) {
	const tag = path.join(candidate, 'CACHEDIR.TAG');
	if (!safeAbsoluteDirectory(candidate) || !safeRegularFile(tag)) return false;
	try { return fs.readFileSync(tag, 'utf8').includes(CACHEDIR_TAG_SIGNATURE); } catch { return false; }
}

// A directory holding git-tracked content is never disposable, whatever it is
// named and whatever it contains. A previous name-matching sweep on a real
// host deleted 548 tracked fixture files that lived under a directory called
// `target`; this predicate is the guard against repeating that.
function containsTrackedFiles(candidate) {
	const result = spawnSync('git', ['-C', candidate, 'ls-files', '--error-unmatch', '.'], { encoding: 'utf8', timeout: GIT_PROBE_TIMEOUT_MS, maxBuffer: MAX_COMMAND_BYTES });
	if (result.error && result.error.code === 'ETIMEDOUT') return true;
	if (typeof result.status !== 'number') return true;
	return result.status === 0 && Boolean((result.stdout || '').trim());
}

// Cargo holds `.cargo-lock` for the duration of a build. Treat a held lock, or
// any recent write, as an active target so a running build is never disturbed.
function cargoTargetActive(candidate, now) {
	const lock = path.join(candidate, '.cargo-lock');
	if (safeRegularFile(lock)) {
		try { if (now - fs.lstatSync(lock).mtimeMs < CARGO_ACTIVE_WINDOW_MS) return true; } catch { return true; }
	}
	return now - cargoTargetLastWrite(candidate) < CARGO_ACTIVE_WINDOW_MS;
}

function cargoTargetAgeDays(candidate, now) { return ageDays(cargoTargetLastWrite(candidate), now); }

function cargoTargetLastWrite(candidate) {
	let latest = 0;
	for (const entry of [candidate, path.join(candidate, 'debug'), path.join(candidate, 'release'), path.join(candidate, 'CACHEDIR.TAG')]) {
		try { latest = Math.max(latest, fs.lstatSync(entry).mtimeMs); } catch { /* absent profiles are not evidence */ }
	}
	return latest;
}

function containedInRoot(candidate, root) {
	try { return inside(fs.realpathSync(candidate), fs.realpathSync(root)) && !fs.lstatSync(candidate).isSymbolicLink(); } catch { return false; }
}

function rootIdFor(root, config) {
	const index = config.temp_roots.indexOf(root);
	return index >= 0 ? `temp-${index}` : 'unmanaged';
}

// Re-verify every safety condition against the filesystem at reclaim time.
// The inventory that produced this id may be seconds or minutes old.
function reclaimCargoTarget(id, config, options) {
	const now = options.now || Date.now();
	for (const root of config.temp_roots) {
		for (const candidate of cargoTargetDirectories(root).targets) {
			if (`${CARGO_TARGET_PREFIX}${digest(candidate).slice(0, 32)}` !== id) continue;
			if (!isCargoTargetDirectory(candidate) || !containedInRoot(candidate, root)) return null;
			if (containsTrackedFiles(candidate) || cargoTargetActive(candidate, now)) return null;
			const bytes = walkBytes(candidate).bytes;
			const quarantine = path.join(path.dirname(candidate), `.${path.basename(candidate)}.homeboy-reclaim-${crypto.randomUUID()}`);
			try {
				fs.renameSync(candidate, quarantine);
				if (!isCargoTargetDirectory(quarantine)) { fs.renameSync(quarantine, candidate); return null; }
				fs.rmSync(quarantine, { recursive: true, force: false });
				return { bytes };
			} catch { return null; }
		}
	}
	return null;
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
		for (const candidate of boundedDirectories(root).directories) {
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
function sign(value, key) { return crypto.createHmac('sha256', key).update(JSON.stringify(value)).digest('hex'); }
function secureEqual(left, right) { return left.length === right.length && crypto.timingSafeEqual(Buffer.from(left), Buffer.from(right)); }
function reclaimable(value) { return value.class === 'scratch' && value.ownership_known && value.reconstructable && !value.active && !value.referenced; }
function processAlive(pid) { if (!Number.isInteger(pid) || pid <= 0) return false; try { process.kill(pid, 0); return true; } catch (error) { return error?.code === 'EPERM'; } }
function safeEntries(directory, limit = MAX_WALK_ENTRIES) { try { const handle = fs.opendirSync(directory, { bufferSize: Math.min(limit, 128) }); const entries = []; for (let entry = handle.readSync(); entry && entries.length < limit; entry = handle.readSync()) entries.push(entry); handle.closeSync(); return entries; } catch { return []; } }
function sizeOf(candidate) { return walkBytes(candidate).bytes; }
function walkBytes(root, known = new Set()) {
	const stack = [[root, 0]]; let bytes = 0; let entries = 0; const reasons = new Set();
	while (stack.length) {
		if (entries >= MAX_WALK_ENTRIES) { reasons.add('entry_limit'); break; }
		if (bytes >= MAX_WALK_BYTES) { reasons.add('byte_limit'); break; }
		const [current, depth] = stack.pop(); entries += 1;
		try { const stat = fs.lstatSync(current); if (stat.isSymbolicLink() || known.has(current)) continue; if (stat.isDirectory()) { if (depth >= MAX_WALK_DEPTH) { reasons.add('depth_limit'); continue; } for (const entry of safeEntries(current, MAX_WALK_ENTRIES - entries)) stack.push([path.join(current, entry.name), depth + 1]); } else bytes += stat.size; } catch { reasons.add('read_error'); }
	}
	return { bytes, entries, reasons: [...reasons] };
}
function unknownBytesBelow(root, known) { return walkBytes(root, known); }
function boundedDirectories(root) {
	const stack = [[root, 0]]; const directories = []; let entries = 0; const reasons = new Set();
	while (stack.length) { if (entries >= MAX_WALK_ENTRIES) { reasons.add('entry_limit'); break; } const [current, depth] = stack.pop(); entries += 1; try { const stat = fs.lstatSync(current); if (stat.isSymbolicLink() || !stat.isDirectory()) continue; directories.push(current); if (depth >= MAX_WALK_DEPTH) { reasons.add('depth_limit'); continue; } for (const entry of safeEntries(current, MAX_WALK_ENTRIES - entries)) if (entry.isDirectory()) stack.push([path.join(current, entry.name), depth + 1]); } catch { reasons.add('read_error'); } }
	return { directories, entries, reasons: [...reasons] };
}
function rootId(candidate, roots) { return roots.find((root) => inside(candidate, root.path))?.id || 'unmanaged'; }
function sameRealDirectory(candidate, root) { try { return inside(fs.realpathSync(candidate), fs.realpathSync(root)) && !fs.lstatSync(candidate).isSymbolicLink(); } catch { return false; } }
function safeRegularFile(candidate) { try { return safeAbsolutePath(candidate) && fs.lstatSync(candidate).isFile() && !fs.lstatSync(candidate).isSymbolicLink(); } catch { return false; } }
function safeAbsoluteDirectory(candidate) { try { return safeAbsolutePath(candidate) && fs.lstatSync(candidate).isDirectory() && !fs.lstatSync(candidate).isSymbolicLink(); } catch { return false; } }
function safeAbsolutePath(value) { return typeof value === 'string' && path.isAbsolute(value) ? path.resolve(value) : ''; }
function absolutePaths(values) { return [...new Set(values.map(safeAbsolutePath).filter(Boolean))]; }
function inside(candidate, root) { if (!candidate || !root) return false; const relative = path.relative(root, candidate); return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative)); }
function overlaps(one, two) { return inside(one, two) || inside(two, one); }
function hasOverlappingRoots(roots) { return roots.some((root, index) => roots.slice(index + 1).some((other) => overlaps(root, other))); }
function fingerprint(candidate) { try { const stat = fs.lstatSync(candidate); return `${stat.dev}:${stat.ino}:${stat.size}:${stat.mtimeMs}`; } catch { return 'missing'; } }
// Bind a reclaim token to the item it authorizes — that item's identity, its
// retention state, and its own on-disk fingerprint — rather than to a digest
// of the whole inventory. A retention root is written continuously by normal
// agent activity, so a root-wide generation changes between inventory and
// reclaim and rejected every request, including ones targeting items that
// never moved (#2832). An item whose own directory changed since inventory
// still fails this check and is left alone.
function reclaimToken(id, state, candidate) { return digest(`${id}:${state}:${fingerprint(candidate)}`); }
function digest(value) { return crypto.createHash('sha256').update(value).digest('hex'); }
function validId(value) { return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value); }
function ageDays(value, now = Date.now()) { const time = typeof value === 'number' ? value : Date.parse(value); return Number.isFinite(time) && time <= now ? Math.floor((now - time) / 86_400_000) : 0; }

module.exports = { CONFIG_ENV, MARKER, PROVIDER_ID, SCHEMA, externalStorageRetentionProviderContract, finalizeOwnershipMarker, handleRequest, retentionConfig, writeOwnershipMarker };
