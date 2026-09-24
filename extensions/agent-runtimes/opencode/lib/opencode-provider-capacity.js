'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { authStorePath, openCodeStoreSecretEnv } = require('./opencode-auth-plan');

const OPENCODE_CAPACITY_TIMEOUT_MS = 5_000;
const OPENCODE_CREDENTIAL_STORE_MAX_BYTES = 256 * 1024;

// Plan usage endpoints for providers that publish them. Each entry is a GET
// that costs no inference and maps its response into percent-used windows.
// Providers without an entry report no capacity, which Homeboy reads as
// "unknown" rather than exhausted.
const CAPACITY_PROVIDERS = {
	anthropic: {
		url: 'https://api.anthropic.com/api/oauth/usage',
		headers: (account) => ({ Authorization: `Bearer ${account.credential}`, 'anthropic-beta': 'oauth-2025-04-20' }),
		windows: anthropicWindows,
		// Pooled OAuth accounts often carry no stored email; the profile names
		// the account so operators know which login to act on.
		profile: {
			url: 'https://api.anthropic.com/api/oauth/profile',
			email: (body) => body?.account?.email_address || body?.account?.email,
		},
	},
	openai: {
		url: 'https://chatgpt.com/backend-api/wham/usage',
		headers: (account) => ({
			Authorization: `Bearer ${account.credential}`,
			...(account.accountId ? { 'ChatGPT-Account-Id': account.accountId } : {}),
		}),
		windows: openAiWindows,
	},
	'zai-coding-plan': {
		url: 'https://api.z.ai/api/monitor/usage/quota/limit',
		headers: (account) => ({ Authorization: account.credential }),
		windows: zaiWindows,
	},
	// OpenCode Zen stores this provider as an API key in the auth store.
	'opencode-go': {
		url: 'https://opencode.ai/zen/go/v1/usage',
		headers: (account) => ({ Authorization: `Bearer ${account.credential}` }),
		windows: opencodeGoWindows,
	},
	// Grok Build's billing endpoint backs the xai plan (see xai-org/grok-build
	// crates/codegen/xai-grok-shell/src/extensions/billing.rs).
	xai: {
		url: 'https://cli-chat-proxy.grok.com/v1/billing?format=credits',
		headers: xaiHeaders,
		windows: xaiWindows,
	},
};

const ANTHROPIC_WINDOWS = ['five_hour', 'seven_day', 'seven_day_opus', 'seven_day_sonnet', 'seven_day_oauth_apps'];
const ZAI_WINDOW_UNITS = { 3: 'hour', 4: 'day', 5: 'month', 6: 'week' };
const OPENCODE_GO_WINDOWS = ['rolling', 'weekly', 'monthly'];

/**
 * Look up plan capacity for one OpenCode provider across every connected
 * account.
 *
 * OpenCode multi-account plugins (for example Kimaki's) keep a pool of OAuth
 * accounts per provider in `<provider>-oauth-accounts.json` beside the auth
 * store and rotate between them, so the route's capacity is the pool's
 * capacity. Without a pool, the single `auth.json` entry is the account.
 *
 * Resolves to null when the provider publishes no usage endpoint or has no
 * stored credential. Otherwise resolves to `{ scope, accounts, capacity?,
 * exhausted, diagnostic? }`: `scope` is the stable, non-secret pool id shared
 * by every route on the provider (`opencode:<provider>`), `accounts` reports
 * each account's state, and `capacity` matches Homeboy's readiness `capacity`
 * contract for the pool (percent remaining on the best available account, or
 * the earliest reset when every account is exhausted).
 */
async function openCodeProviderCapacity(provider, options = {}) {
	const source = CAPACITY_PROVIDERS[provider];
	if (!source) return null;
	const env = options.env || process.env;
	const accounts = providerAccounts(provider, env, options.fs || fs);
	if (!accounts.length) return null;
	const now = options.now || Date.now();
	const reports = await Promise.all(accounts.map((account) => accountCapacity(source, provider, account, now, options)));
	return summarizePool(provider, reports);
}

async function accountCapacity(source, provider, account, now, options) {
	if (account.expires && account.expires <= now) {
		// Refreshing would rotate a token another tool owns; report it instead.
		return { account: account.label, state: 'credential_expired' };
	}
	const [label, usage] = await Promise.all([
		accountLabel(source, account, options),
		accountUsage(source, provider, account, options),
	]);
	return { account: label, ...usage };
}

async function accountLabel(source, account, options) {
	if (account.named || !source.profile) return account.label;
	try {
		const response = await (options.fetch || globalThis.fetch)(source.profile.url, {
			method: 'GET',
			headers: { Accept: 'application/json', ...source.headers(account) },
			signal: AbortSignal.timeout(options.timeoutMs || OPENCODE_CAPACITY_TIMEOUT_MS),
		});
		if (!response.ok) return account.label;
		const email = source.profile.email(await response.json());
		return typeof email === 'string' && email.trim() ? email.trim() : account.label;
	} catch {
		return account.label;
	}
}

async function accountUsage(source, provider, account, options) {
	const base = {};
	const fetchImpl = options.fetch || globalThis.fetch;
	let body;
	try {
		const response = await fetchImpl(source.url, {
			method: 'GET',
			headers: { Accept: 'application/json', ...source.headers(account) },
			signal: AbortSignal.timeout(options.timeoutMs || OPENCODE_CAPACITY_TIMEOUT_MS),
		});
		if (!response.ok) {
			return { ...base, state: response.status === 401 || response.status === 403 ? 'credential_rejected' : 'lookup_failed', diagnostic: `${provider} usage lookup returned HTTP ${response.status}` };
		}
		body = await response.json();
	} catch (error) {
		return { ...base, state: 'lookup_failed', diagnostic: `${provider} usage lookup failed: ${error?.name === 'TimeoutError' ? 'timed out' : 'request error'}` };
	}
	let windows;
	try {
		windows = source.windows(body);
	} catch {
		windows = [];
	}
	if (!windows.length) {
		return { ...base, state: 'lookup_failed', diagnostic: `${provider} usage response did not include any recognized usage windows` };
	}
	const [tightest] = [...windows].sort((left, right) => (
		right.used_percent - left.used_percent || resetOrder(left) - resetOrder(right)
	));
	const exhausted = windows.some((window) => window.exhausted);
	// Exhaustion lasts until the latest exhausted window resets.
	const resetAt = exhausted
		? windows.filter((window) => window.exhausted).map((window) => window.reset_at).filter(Boolean).sort().pop() || null
		: tightest.reset_at;
	return {
		...base,
		state: exhausted ? 'exhausted' : 'available',
		remaining: exhausted ? 0 : Math.max(0, round(100 - tightest.used_percent)),
		reset_at: resetAt,
		windows,
	};
}

function summarizePool(provider, accounts) {
	const measured = accounts.filter((account) => account.state === 'available' || account.state === 'exhausted');
	const result = { scope: capacityScope(provider), accounts, exhausted: false };
	if (!measured.length) {
		result.diagnostic = `${provider} usage could not be read for any of ${accounts.length} connected account(s)`;
		return result;
	}
	const available = measured.filter((account) => account.state === 'available');
	if (available.length) {
		const [best] = [...available].sort((left, right) => right.remaining - left.remaining || resetOrder(left) - resetOrder(right));
		result.capacity = capacityObject(best.remaining, best.reset_at);
		return result;
	}
	// Every measured account is exhausted. The pool frees up at the earliest
	// reset. Accounts that could not be measured might still have capacity,
	// so only a fully measured pool is declared exhausted.
	const [soonest] = [...measured].sort((left, right) => resetOrder(left) - resetOrder(right));
	result.capacity = capacityObject(0, soonest.reset_at);
	result.exhausted = measured.length === accounts.length;
	if (!result.exhausted) {
		result.diagnostic = `${accounts.length - measured.length} of ${accounts.length} ${provider} account(s) could not be measured`;
	}
	return result;
}

function capacityObject(remaining, resetAt) {
	const capacity = { remaining, limit: 100, unit: 'percent' };
	if (resetAt) capacity.reset_at = resetAt;
	return capacity;
}

function capacityScope(provider) {
	return `opencode:${provider}`;
}

function anthropicWindows(body) {
	return ANTHROPIC_WINDOWS
		.filter((name) => finiteNumber(body?.[name]?.utilization))
		.map((name) => percentWindow(name, body[name].utilization, isoTime(body[name].resets_at)));
}

function openAiWindows(body) {
	const limit = body?.rate_limit;
	const blocked = limit?.allowed === false || limit?.limit_reached === true;
	const windows = [['primary_window', limit?.primary_window], ['secondary_window', limit?.secondary_window]]
		.filter(([, window]) => finiteNumber(window?.used_percent))
		.map(([name, window]) => percentWindow(
			windowName(window.limit_window_seconds) || name,
			window.used_percent,
			isoTime(finiteNumber(window.reset_at) ? window.reset_at * 1000 : null),
		));
	if (blocked && windows.length && !windows.some((window) => window.exhausted)) {
		// The account is blocked even though no window reads 100%: report the
		// fullest window as the exhausted one so the verdict stays truthful.
		const [fullest] = [...windows].sort((left, right) => right.used_percent - left.used_percent);
		fullest.exhausted = true;
	}
	return windows;
}

function zaiWindows(body) {
	return (Array.isArray(body?.data?.limits) ? body.data.limits : [])
		.filter((limit) => limit?.type === 'TOKENS_LIMIT' && finiteNumber(limit.percentage))
		.map((limit) => percentWindow(
			`tokens_${limit.number}_${ZAI_WINDOW_UNITS[limit.unit] || `unit${limit.unit}`}`,
			limit.percentage,
			isoTime(limit.nextResetTime),
		));
}

function opencodeGoWindows(body) {
	return OPENCODE_GO_WINDOWS
		.filter((name) => body?.usage?.[name] && typeof body.usage[name] === 'object')
		.map((name) => {
			const window = body.usage[name];
			const mapped = percentWindow(name, finiteNumber(window.percent) ? window.percent : 0, isoTime(window.resetsAt));
			// A rate-limited window is spent even when its percent has not
			// caught up to 100 yet.
			if (window.status === 'rate-limited') mapped.exhausted = true;
			return mapped;
		});
}

function xaiHeaders(account) {
	const headers = {
		Authorization: `Bearer ${account.credential}`,
		'X-XAI-Token-Auth': 'xai-grok-cli',
	};
	const userId = xaiUserId(account);
	if (userId) headers['x-userid'] = userId;
	return headers;
}

// The billing endpoint wants the account id: pooled entries store it as
// accountId, and a single store login carries it as principal_id inside the
// access token's JWT payload. The token itself is never logged or reported.
function xaiUserId(account) {
	if (account.accountId) return account.accountId;
	try {
		const payload = JSON.parse(Buffer.from(account.credential.split('.')[1], 'base64url').toString('utf8'));
		return typeof payload?.principal_id === 'string' && payload.principal_id ? payload.principal_id : null;
	} catch {
		return null;
	}
}

function xaiWindows(body) {
	const config = body?.config;
	if (!config || typeof config !== 'object') return [];
	const period = config.currentPeriod && typeof config.currentPeriod === 'object' ? config.currentPeriod : {};
	return [percentWindow(
		period.type === 'USAGE_PERIOD_TYPE_WEEKLY' ? 'credits_weekly' : 'credits',
		xaiUsedPercent(config),
		isoTime(period.end || config.billingPeriodEnd),
	)];
}

// Mirrors Grok Build's credit_balance_from_config: a present usage percent
// wins, then the used/limit ratio. Proto JSON omits zero values, so anything
// else reads as 0% used.
function xaiUsedPercent(config) {
	if (finiteNumber(config.creditUsagePercent)) return config.creditUsagePercent;
	const limit = config.monthlyLimit?.val;
	if (finiteNumber(limit) && limit > 0) {
		return (finiteNumber(config.used?.val) ? config.used.val : 0) / limit * 100;
	}
	return 0;
}

function percentWindow(name, usedPercent, resetAt) {
	const used = Math.min(100, Math.max(0, round(usedPercent)));
	return { name, used_percent: used, reset_at: resetAt, exhausted: used >= 100 };
}

function windowName(seconds) {
	if (!finiteNumber(seconds) || seconds <= 0) return '';
	if (seconds % 86_400 === 0) return `${seconds / 86_400}_day`;
	if (seconds % 3_600 === 0) return `${seconds / 3_600}_hour`;
	return '';
}

/**
 * Every connected account for a provider, as `{ label, credential,
 * accountId?, expires? }`. Labels never contain credentials.
 */
function providerAccounts(provider, env, fileSystem) {
	const storePath = authStorePath(env);
	const pool = readJson(path.join(path.dirname(storePath), `${provider}-oauth-accounts.json`), fileSystem);
	const pooled = (Array.isArray(pool?.accounts) ? pool.accounts : [])
		.map((entry, index) => oauthAccount(entry, entry?.email || `${provider}#${index}`, Boolean(entry?.email)))
		.filter(Boolean);
	if (pooled.length) return pooled;

	const entry = readJson(storePath, fileSystem)?.[provider];
	if (entry?.type === 'api' && typeof entry.key === 'string' && entry.key) {
		return [{ label: provider, credential: entry.key }];
	}
	const stored = oauthAccount(entry, provider);
	if (stored) return [stored];
	// Lab runners materialize OAuth store secrets as AI_PROVIDER_OPENCODE_<PROVIDER>_ACCESS.
	const accessEnv = openCodeStoreSecretEnv(provider).find((name) => name.endsWith('_ACCESS'));
	return accessEnv && typeof env[accessEnv] === 'string' && env[accessEnv]
		? [{ label: provider, credential: env[accessEnv] }]
		: [];
}

function oauthAccount(entry, label, named = false) {
	if (entry?.type !== 'oauth' || typeof entry.access !== 'string' || !entry.access) return null;
	const expires = Number(entry.expires);
	return {
		label: String(label),
		named,
		credential: entry.access,
		...(typeof entry.accountId === 'string' && entry.accountId ? { accountId: entry.accountId } : {}),
		...(Number.isFinite(expires) && expires > 0 ? { expires } : {}),
	};
}

function readJson(file, fileSystem) {
	try {
		const stat = fileSystem.statSync(file);
		if (!stat.isFile() || stat.size > OPENCODE_CREDENTIAL_STORE_MAX_BYTES) return null;
		const value = JSON.parse(fileSystem.readFileSync(file, 'utf8'));
		return value && typeof value === 'object' ? value : null;
	} catch {
		return null;
	}
}

function isoTime(value) {
	if (value === null || value === undefined || value === '') return null;
	const date = new Date(value);
	return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

function resetOrder(entry) {
	return entry.reset_at ? Date.parse(entry.reset_at) : Number.POSITIVE_INFINITY;
}

function finiteNumber(value) {
	return typeof value === 'number' && Number.isFinite(value);
}

function round(value) {
	return Math.round(value * 100) / 100;
}

module.exports = {
	OPENCODE_CAPACITY_TIMEOUT_MS,
	openCodeProviderCapacity,
};
