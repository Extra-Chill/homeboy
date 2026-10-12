'use strict';

// Controller-side Anthropic OAuth refresh for the `anthropic-oauth` account
// (homeboy#15783).
//
// Homeboy runs this on the controller before delivering the account to a Lab
// runner. The runner receives access + expiry only, so this host is the sole
// owner of the single-use refresh chain. The job here is to leave
// ~/.local/share/opencode/auth.json holding an access token that outlives a
// Lab run, or to exit non-zero with a message an operator can act on.
//
// It shares the on-disk contract of the OpenCode Anthropic auth plugins
// (Kimaki's and claude-code-auth.ts): the `${auth.json}.lock` directory with a
// 30s stale window and 30s wait, `auth.json`'s `anthropic` entry, and the
// identity-preserving records in `anthropic-oauth-accounts.json`. A refresh
// here is therefore seen, not raced, by a plugin refreshing on the same host.

const fs = require('node:fs');
const path = require('node:path');

const CLIENT_ID = '9d1c250a-e61b-44d9-88ed-5944d1962f5e';
const TOKEN_URL = 'https://platform.claude.com/v1/oauth/token';
// A delivered token must outlive the Lab run that receives it. Anthropic access
// tokens last about eight hours; refresh whenever less than this remains.
const MIN_REMAINING_MS = 4 * 60 * 60 * 1000;
// Match the plugins: they record expiry five minutes early.
const EXPIRY_SKEW_MS = 5 * 60 * 1000;
const AUTH_LOCK_STALE_MS = 30_000;
const AUTH_LOCK_TIMEOUT_MS = 30_000;
const REQUEST_TIMEOUT_MS = 20_000;

class RefreshError extends Error {}

function opencodeDataDir(env = process.env) {
	return path.join(env.XDG_DATA_HOME || path.join(env.HOME || '', '.local', 'share'), 'opencode');
}

function readJson(filePath, fallback) {
	try {
		return JSON.parse(fs.readFileSync(filePath, 'utf8'));
	} catch (error) {
		if (error.code === 'ENOENT') return fallback;
		throw new RefreshError(`${path.basename(filePath)} is not readable JSON: ${error.message}`);
	}
}

function writeJsonAtomic(filePath, value) {
	const temporary = `${filePath}.${process.pid}.tmp`;
	fs.writeFileSync(temporary, JSON.stringify(value, null, 2), { mode: 0o600 });
	fs.renameSync(temporary, filePath);
	fs.chmodSync(filePath, 0o600);
}

function remainingMs(entry, now) {
	return typeof entry?.access === 'string' && entry.access !== '' && typeof entry.expires === 'number'
		? entry.expires - now
		: -Infinity;
}

async function withAuthLock(authPath, action, { now = Date.now, sleep = defaultSleep } = {}) {
	const lockDir = `${authPath}.lock`;
	const deadline = now() + AUTH_LOCK_TIMEOUT_MS;
	fs.mkdirSync(path.dirname(authPath), { recursive: true });
	for (;;) {
		try {
			fs.mkdirSync(lockDir);
			break;
		} catch (error) {
			if (error.code !== 'EEXIST') throw error;
			let stat;
			try {
				stat = fs.statSync(lockDir);
			} catch {
				continue;
			}
			if (now() - stat.mtimeMs > AUTH_LOCK_STALE_MS) {
				fs.rmSync(lockDir, { recursive: true, force: true });
				continue;
			}
			if (now() >= deadline) throw new RefreshError(`timed out waiting for the auth lock ${lockDir}`);
			await sleep(100);
		}
	}
	try {
		return await action();
	} finally {
		fs.rmSync(lockDir, { recursive: true, force: true });
	}
}

function defaultSleep(ms) {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

async function requestRefresh(refreshToken, { fetchImpl = globalThis.fetch, now = Date.now } = {}) {
	const controller = new AbortController();
	const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
	let response;
	try {
		response = await fetchImpl(TOKEN_URL, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
			body: JSON.stringify({ grant_type: 'refresh_token', refresh_token: refreshToken, client_id: CLIENT_ID }),
			signal: controller.signal,
		});
	} catch (error) {
		throw new RefreshError(`Anthropic token endpoint unreachable: ${error.message}`);
	} finally {
		clearTimeout(timer);
	}
	const text = await response.text();
	let data = {};
	try {
		data = JSON.parse(text);
	} catch {
		// Reported below without echoing the body, which may carry tokens.
	}
	if (!response.ok) {
		const reason = typeof data.error === 'string' ? data.error : data.error?.type || 'unknown_error';
		const hint = reason === 'invalid_grant'
			? ' The stored login can no longer be refreshed; run `opencode auth login` on the controller.'
			: '';
		throw new RefreshError(`Anthropic token refresh failed with HTTP ${response.status} (${reason}).${hint}`);
	}
	if (typeof data.access_token !== 'string' || typeof data.refresh_token !== 'string') {
		throw new RefreshError('Anthropic token refresh returned no access/refresh token.');
	}
	return {
		type: 'oauth',
		refresh: data.refresh_token,
		access: data.access_token,
		expires: now() + (Number(data.expires_in) || 3600) * 1000 - EXPIRY_SKEW_MS,
	};
}

// Keep the plugins' account pool in step: the record that held the rotated
// token now holds its replacement, so neither plugin later retries a dead one.
function replaceAccountRecord(accountsPath, previous, next) {
	const store = readJson(accountsPath, null);
	if (!store || !Array.isArray(store.accounts)) return;
	const index = store.accounts.findIndex((account) =>
		account && (account.refresh === previous.refresh || account.access === previous.access));
	if (index < 0) return;
	store.accounts[index] = {
		...store.accounts[index],
		refresh: next.refresh,
		access: next.access,
		expires: next.expires,
	};
	writeJsonAtomic(accountsPath, store);
}

/**
 * Leave the controller's Anthropic OAuth login with at least MIN_REMAINING_MS
 * of access-token life. Returns `{ refreshed }`; throws RefreshError with an
 * operator-facing message (never a token) when that is not possible.
 */
async function ensureFreshAnthropicLogin(options = {}) {
	const env = options.env || process.env;
	const now = options.now || Date.now;
	const dataDir = opencodeDataDir(env);
	const authPath = path.join(dataDir, 'auth.json');
	const accountsPath = path.join(dataDir, 'anthropic-oauth-accounts.json');

	const current = readJson(authPath, {})?.anthropic;
	if (!current || current.type !== 'oauth') {
		throw new RefreshError(`No Anthropic OAuth login in ${authPath}; run \`opencode auth login\` on the controller.`);
	}
	if (remainingMs(current, now()) >= MIN_REMAINING_MS) return { refreshed: false };

	return withAuthLock(authPath, async () => {
		// Another process may have refreshed while this one waited.
		const auth = readJson(authPath, {});
		const latest = auth.anthropic;
		if (!latest || latest.type !== 'oauth') {
			throw new RefreshError(`No Anthropic OAuth login in ${authPath}; run \`opencode auth login\` on the controller.`);
		}
		if (remainingMs(latest, now()) >= MIN_REMAINING_MS) return { refreshed: false };
		if (typeof latest.refresh !== 'string' || latest.refresh === '') {
			throw new RefreshError('The controller\'s Anthropic login has no refresh token; run `opencode auth login` on the controller.');
		}
		const next = await requestRefresh(latest.refresh, { fetchImpl: options.fetch, now });
		auth.anthropic = { ...latest, ...next };
		writeJsonAtomic(authPath, auth);
		replaceAccountRecord(accountsPath, latest, next);
		return { refreshed: true };
	}, { now, sleep: options.sleep });
}

module.exports = {
	AUTH_LOCK_STALE_MS,
	MIN_REMAINING_MS,
	RefreshError,
	TOKEN_URL,
	ensureFreshAnthropicLogin,
};
