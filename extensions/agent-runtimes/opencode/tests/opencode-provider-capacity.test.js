'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { openCodeProviderCapacity } = require('../lib/opencode-provider-capacity');
const { openCodeProviderReadiness } = require('..');
const fixtures = require('./fixtures/provider-capacity.json');

const NOW = Date.parse('2026-09-24T12:00:00Z');
const FUTURE = NOW + 3_600_000;

// A minimal JWT-shaped access token so xai account ids can be read from the
// payload the way the real CLI tokens carry them.
const jwt = (payload) => [
	Buffer.from('{"alg":"RS256","typ":"JWT"}').toString('base64url'),
	Buffer.from(JSON.stringify(payload)).toString('base64url'),
	'signature',
].join('.');
const XAI_ACCESS = jwt({ sub: 'user-9', principal_id: 'principal-123' });

function dataHome(authStore, pools = {}) {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opencode-capacity-'));
	fs.mkdirSync(path.join(root, 'opencode'), { recursive: true });
	fs.writeFileSync(path.join(root, 'opencode', 'auth.json'), JSON.stringify(authStore));
	for (const [provider, accounts] of Object.entries(pools)) {
		fs.writeFileSync(path.join(root, 'opencode', `${provider}-oauth-accounts.json`), JSON.stringify({ version: 1, activeIndex: 0, accounts }));
	}
	return { HOME: root, XDG_DATA_HOME: root, PATH: process.env.PATH };
}

const singleAccountEnv = dataHome({
	anthropic: { type: 'oauth', access: 'anthropic-access-secret', refresh: 'r', expires: FUTURE },
	openai: { type: 'oauth', access: 'openai-access-secret', refresh: 'r', expires: FUTURE },
	'zai-coding-plan': { type: 'api', key: 'zai-key-secret' },
	'opencode-go': { type: 'api', key: 'go-key-secret' },
	xai: { type: 'oauth', access: XAI_ACCESS, refresh: 'r', expires: FUTURE },
});

const PROFILE_URL = 'https://api.anthropic.com/api/oauth/profile';

// Profile lookups answer from `profiles` (keyed by credential) and are kept
// out of `calls`, which records usage requests only.
function profileResponse(url, init, profiles) {
	if (url !== PROFILE_URL) return null;
	const credential = String(init.headers.Authorization).replace(/^Bearer /, '');
	const email = profiles[credential];
	return email
		? { ok: true, status: 200, json: async () => ({ account: { email_address: email } }) }
		: { ok: false, status: 404, json: async () => ({}) };
}

// Responds per credential so pooled accounts can carry different usage.
function fetchByCredential(responses, calls = [], profiles = {}) {
	return async (url, init) => {
		const profile = profileResponse(url, init, profiles);
		if (profile) return profile;
		calls.push({ url, init });
		const credential = String(init.headers.Authorization).replace(/^Bearer /, '');
		const [status, body] = responses[credential] || [500, {}];
		return { ok: status >= 200 && status < 300, status, json: async () => body };
	};
}

function fetchReturning(body, status = 200, calls = []) {
	return async (url, init) => {
		const profile = profileResponse(url, init, {});
		if (profile) return profile;
		calls.push({ url, init });
		return { ok: status >= 200 && status < 300, status, json: async () => body };
	};
}

async function singleAccountTests() {
	const env = singleAccountEnv;
	const options = (fetch) => ({ env, fetch, now: NOW });

	// Anthropic: tightest window is the 5-hour one; credential goes in a Bearer header.
	const anthropicCalls = [];
	const anthropic = await openCodeProviderCapacity('anthropic', options(fetchReturning(fixtures.anthropic, 200, anthropicCalls)));
	assert.equal(anthropicCalls[0].url, 'https://api.anthropic.com/api/oauth/usage');
	assert.equal(anthropicCalls[0].init.headers.Authorization, 'Bearer anthropic-access-secret');
	assert.equal(anthropicCalls[0].init.headers['anthropic-beta'], 'oauth-2025-04-20');
	assert.deepEqual(anthropic.capacity, { remaining: 96, limit: 100, unit: 'percent', reset_at: '2026-09-24T15:50:00.400Z' });
	assert.equal(anthropic.scope, 'opencode:anthropic');
	assert.equal(anthropic.accounts[0].account, 'anthropic');
	assert.deepEqual(anthropic.accounts[0].windows.map((window) => window.name), ['five_hour', 'seven_day']);
	assert.equal(anthropic.exhausted, false);

	// OpenAI: an exhausted weekly window reports capacity 0 with its reset time.
	const openai = await openCodeProviderCapacity('openai', options(fetchReturning(fixtures.openai_exhausted)));
	assert.deepEqual(openai.capacity, { remaining: 0, limit: 100, unit: 'percent', reset_at: '2026-09-28T23:29:35.000Z' });
	assert.equal(openai.accounts[0].windows[0].name, '7_day');
	assert.equal(openai.exhausted, true);

	// OpenAI: a blocked account is exhausted even when no window reads 100%.
	const blocked = await openCodeProviderCapacity('openai', options(fetchReturning(fixtures.openai_blocked_below_full)));
	assert.equal(blocked.exhausted, true);

	// Z.ai: only token windows count; a window without nextResetTime has no reset_at.
	const zaiCalls = [];
	const zai = await openCodeProviderCapacity('zai-coding-plan', options(fetchReturning(fixtures.zai, 200, zaiCalls)));
	assert.equal(zaiCalls[0].init.headers.Authorization, 'zai-key-secret');
	assert.deepEqual(zai.accounts[0].windows.map((window) => [window.name, window.used_percent, window.reset_at]), [
		['tokens_5_hour', 0, null],
		['tokens_1_week', 58, '2026-09-29T02:00:15.973Z'],
	]);
	assert.deepEqual(zai.capacity, { remaining: 42, limit: 100, unit: 'percent', reset_at: '2026-09-29T02:00:15.973Z' });

	// Providers without a usage endpoint, or without a credential, report nothing.
	let called = false;
	const noCall = async () => { called = true; };
	assert.equal(await openCodeProviderCapacity('google', options(noCall)), null);
	assert.equal(await openCodeProviderCapacity('anthropic', { env: { HOME: '/nonexistent' }, fetch: noCall, now: NOW }), null);
	assert.equal(called, false);

	// Lab runners provide OAuth access tokens through the store secret env.
	const envCalls = [];
	await openCodeProviderCapacity('openai', {
		env: { HOME: '/nonexistent', AI_PROVIDER_OPENCODE_OPENAI_ACCESS: 'env-access-secret' },
		fetch: fetchReturning(fixtures.openai_exhausted, 200, envCalls),
		now: NOW,
	});
	assert.equal(envCalls[0].init.headers.Authorization, 'Bearer env-access-secret');

	// Failures degrade to a diagnostic without leaking credentials.
	const httpFailure = await openCodeProviderCapacity('anthropic', options(fetchReturning({}, 401)));
	assert.equal(httpFailure.capacity, undefined);
	assert.equal(httpFailure.accounts[0].state, 'credential_rejected');
	assert.match(httpFailure.diagnostic, /could not be read for any of 1/);
	const networkFailure = await openCodeProviderCapacity('anthropic', options(async () => { throw new Error('anthropic-access-secret'); }));
	assert.equal(networkFailure.accounts[0].diagnostic, 'anthropic usage lookup failed: request error');
	assert.ok(!JSON.stringify(networkFailure).includes('anthropic-access-secret'));
	const unrecognized = await openCodeProviderCapacity('anthropic', options(fetchReturning({ unexpected: true })));
	assert.equal(unrecognized.accounts[0].state, 'lookup_failed');
}

async function opencodeGoAndXaiTests() {
	const env = singleAccountEnv;
	const options = (fetch) => ({ env, fetch, now: NOW });

	// opencode-go: the auth-store API key goes out as a Bearer header; the
	// rate-limited monthly window exhausts the account at its own reset.
	const goCalls = [];
	const go = await openCodeProviderCapacity('opencode-go', options(fetchReturning(fixtures.opencode_go, 200, goCalls)));
	assert.equal(goCalls[0].url, 'https://opencode.ai/zen/go/v1/usage');
	assert.equal(goCalls[0].init.headers.Authorization, 'Bearer go-key-secret');
	assert.equal(go.scope, 'opencode:opencode-go');
	assert.deepEqual(go.accounts[0].windows.map((window) => [window.name, window.used_percent, window.reset_at]), [
		['rolling', 0, '2026-09-25T03:58:42.116Z'],
		['weekly', 0, '2026-09-28T00:00:00.000Z'],
		['monthly', 100, '2026-10-08T00:46:00.000Z'],
	]);
	assert.equal(go.accounts[0].state, 'exhausted');
	assert.deepEqual(go.capacity, { remaining: 0, limit: 100, unit: 'percent', reset_at: '2026-10-08T00:46:00.000Z' });

	// A rate-limited window is exhausted even when its percent is below 100.
	const goLimited = await openCodeProviderCapacity('opencode-go', options(fetchReturning(fixtures.opencode_go_rate_limited_below_full)));
	assert.equal(goLimited.accounts[0].windows[1].name, 'monthly');
	assert.equal(goLimited.accounts[0].windows[1].exhausted, true);
	assert.equal(goLimited.exhausted, true);
	assert.equal(goLimited.capacity.remaining, 0);

	// xai: the billing endpoint names the account from the access token's JWT
	// payload, and the token itself never leaks into the report.
	const xaiCalls = [];
	const xai = await openCodeProviderCapacity('xai', options(fetchReturning(fixtures.xai, 200, xaiCalls)));
	assert.equal(xaiCalls[0].url, 'https://cli-chat-proxy.grok.com/v1/billing?format=credits');
	assert.equal(xaiCalls[0].init.headers.Authorization, `Bearer ${XAI_ACCESS}`);
	assert.equal(xaiCalls[0].init.headers['X-XAI-Token-Auth'], 'xai-grok-cli');
	assert.equal(xaiCalls[0].init.headers['x-userid'], 'principal-123');
	assert.deepEqual(xai.accounts[0].windows, [
		{ name: 'credits_weekly', used_percent: 0, reset_at: '2026-10-01T21:08:53.920Z', exhausted: false },
	]);
	assert.deepEqual(xai.capacity, { remaining: 100, limit: 100, unit: 'percent', reset_at: '2026-10-01T21:08:53.920Z' });

	// Without creditUsagePercent the used/limit ratio is the percent.
	const ratio = await openCodeProviderCapacity('xai', options(fetchReturning(fixtures.xai_ratio)));
	assert.equal(ratio.accounts[0].windows[0].used_percent, 84);
	assert.deepEqual(ratio.capacity, { remaining: 16, limit: 100, unit: 'percent', reset_at: '2026-10-01T21:08:53.920Z' });

	// A present usage percent wins and is clamped; without a current period the
	// window falls back to the `credits` name and the billing period end.
	const over = await openCodeProviderCapacity('xai', options(fetchReturning(fixtures.xai_usage_percent_over)));
	assert.equal(over.accounts[0].windows[0].name, 'credits');
	assert.equal(over.accounts[0].windows[0].used_percent, 100);
	assert.equal(over.accounts[0].windows[0].exhausted, true);
	assert.equal(over.capacity.reset_at, '2026-09-30T00:00:00.000Z');

	// A billing response without config reports no recognized windows.
	const noConfig = await openCodeProviderCapacity('xai', options(fetchReturning({ unexpected: true })));
	assert.equal(noConfig.accounts[0].state, 'lookup_failed');

	const reported = JSON.stringify([go, goLimited, xai, ratio, over, noConfig]);
	assert.ok(!reported.includes('go-key-secret'));
	assert.ok(!reported.includes(XAI_ACCESS));
}

async function poolTests() {
	const oauth = (access, extra = {}) => ({ type: 'oauth', access, refresh: `${access}-refresh`, expires: FUTURE, ...extra });
	const weeklySpent = (resetsAt) => ({ five_hour: { utilization: 0, resets_at: null }, seven_day: { utilization: 100, resets_at: resetsAt } });

	// Four Claude plans plus a stale login: three spent for the week, one healthy.
	const env = dataHome({ anthropic: oauth('active') }, {
		anthropic: [
			oauth('stale', { expires: NOW - 3 * 86_400_000, lastUsed: NOW - 3 * 86_400_000 - 1_800_000, email: 'old@example.com' }),
			oauth('plan-a'),
			oauth('plan-b'),
			oauth('plan-c'),
			oauth('active'),
		],
		openai: [
			oauth('codex-1', { email: 'one@example.com', accountId: 'acct-1' }),
			oauth('codex-2', { email: 'two@example.com', accountId: 'acct-2' }),
		],
		xai: [
			{ type: 'oauth', access: jwt({ principal_id: 'pool-principal' }), refresh: 'pool-refresh', expires: FUTURE },
			oauth('grok-acct', { email: 'grok@example.com', accountId: 'acct-77' }),
		],
	});
	const calls = [];
	const pool = await openCodeProviderCapacity('anthropic', {
		env,
		now: NOW,
		fetch: fetchByCredential({
			'plan-a': [200, weeklySpent('2026-09-25T03:00:00Z')],
			'plan-b': [200, weeklySpent('2026-09-29T01:00:00Z')],
			'plan-c': [200, weeklySpent('2026-09-29T16:00:00Z')],
			active: [200, fixtures.anthropic],
		}, calls, { 'plan-b': 'plan-b@example.com', stale: 'never-looked-up@example.com' }),
	});
	assert.equal(calls.length, 4, 'expired pooled credentials are reported without a request');
	assert.deepEqual(pool.accounts.map((account) => [account.account, account.state]), [
		['old@example.com', 'unverified'],
		['anthropic#1', 'exhausted'],
		['plan-b@example.com', 'exhausted'],
		['anthropic#3', 'exhausted'],
		['anthropic#4', 'available'],
	]);
	// An expired access token on an idle pool account is not evidence the
	// account is dead: it is reported as unverified with staleness facts.
	assert.equal(pool.accounts[0].reason, 'access_token_expired');
	assert.equal(pool.accounts[0].token_expired_at, new Date(NOW - 3 * 86_400_000).toISOString());
	assert.equal(pool.accounts[0].last_used_at, new Date(NOW - 3 * 86_400_000 - 1_800_000).toISOString());
	assert.match(pool.accounts[0].diagnostic, /expired 3d ago; not probed/);
	assert.ok(!('credential' in pool.accounts[0]) && !JSON.stringify(pool.accounts[0]).includes('stale'), 'unverified report never carries the credential');
	assert.equal(pool.accounts[1].reset_at, '2026-09-25T03:00:00.000Z');
	assert.equal(pool.exhausted, false);
	assert.equal(pool.scope, 'opencode:anthropic');
	assert.deepEqual(pool.capacity, { remaining: 96, limit: 100, unit: 'percent', reset_at: '2026-09-24T15:50:00.400Z' });

	// Both Codex plans spent: the pool is exhausted until the earliest reset.
	const codexCalls = [];
	const codex = await openCodeProviderCapacity('openai', {
		env,
		now: NOW,
		fetch: fetchByCredential({
			'codex-1': [200, { rate_limit: { allowed: false, limit_reached: true, primary_window: { used_percent: 100, limit_window_seconds: 604800, reset_at: 1790522770 } } }],
			'codex-2': [200, fixtures.openai_exhausted],
		}, codexCalls),
	});
	assert.deepEqual(codexCalls.map((call) => call.init.headers['ChatGPT-Account-Id']), ['acct-1', 'acct-2']);
	assert.equal(codex.exhausted, true);
	assert.equal(codex.scope, 'opencode:openai');
	assert.deepEqual(codex.accounts.map((account) => [account.account, account.reset_at]), [
		['one@example.com', '2026-09-27T15:26:10.000Z'],
		['two@example.com', '2026-09-28T23:29:35.000Z'],
	]);
	assert.deepEqual(codex.capacity, { remaining: 0, limit: 100, unit: 'percent', reset_at: '2026-09-27T15:26:10.000Z' });

	// Pooled Grok logins: the x-userid header prefers the stored accountId and
	// falls back to the access token's JWT payload; labels keep the pool shape.
	const grokCalls = [];
	const grok = await openCodeProviderCapacity('xai', {
		env,
		now: NOW,
		fetch: fetchByCredential({ 'grok-acct': [200, fixtures.xai], [jwt({ principal_id: 'pool-principal' })]: [200, fixtures.xai] }, grokCalls),
	});
	assert.deepEqual(grokCalls.map((call) => call.init.headers['x-userid']), ['pool-principal', 'acct-77']);
	assert.deepEqual(grok.accounts.map((account) => [account.account, account.state]), [
		['xai#0', 'available'],
		['grok@example.com', 'available'],
	]);
	assert.deepEqual(grok.capacity, { remaining: 100, limit: 100, unit: 'percent', reset_at: '2026-10-01T21:08:53.920Z' });

	// An unmeasurable account keeps an otherwise spent pool from being declared exhausted.
	const partial = await openCodeProviderCapacity('openai', {
		env,
		now: NOW,
		fetch: fetchByCredential({ 'codex-1': [200, fixtures.openai_exhausted], 'codex-2': [503, {}] }),
	});
	assert.equal(partial.exhausted, false);
	assert.equal(partial.capacity.remaining, 0);
	assert.match(partial.diagnostic, /1 of 2 openai account/);
	assert.ok(!JSON.stringify([pool, codex, partial, grok]).match(/codex-1|plan-a|refresh|pool-principal/));
}

async function readinessIntegration() {
	const env = singleAccountEnv;
	const executable = path.join(env.HOME, 'opencode-bin');
	fs.writeFileSync(executable, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
	const request = {
		schema: 'homeboy/agent-task-provider-readiness-request/v1',
		effective_config: { runtime_bin: executable, model: 'openai/gpt-5.6-terra' },
	};
	const commands = [];
	const spawnSync = (command, args) => {
		commands.push(args);
		if (args.includes('--version')) return { status: 0, stdout: '1.0.0\n', stderr: '' };
		if (args.includes('auth')) return { status: 0, stdout: 'OpenAI oauth\n', stderr: '' };
		if (args.includes('models')) return { status: 0, stdout: 'openai/gpt-5.6-terra\n', stderr: '' };
		return { status: 0, stdout: '{"type":"text","text":"READY"}\n', stderr: '' };
	};

	// Exhausted plan: classified as capacity, skips the inference probe, carries the reset.
	const exhausted = await openCodeProviderReadiness(request, { env, now: NOW, spawnSync, fetch: fetchReturning(fixtures.openai_exhausted) });
	assert.equal(exhausted.ready, false);
	assert.equal(exhausted.classification, 'capacity');
	assert.equal(exhausted.reason, 'provider_capacity_exhausted');
	assert.equal(exhausted.capacity.scope, 'opencode:openai');
	assert.equal(exhausted.capacity.reset_at, '2026-09-28T23:29:35.000Z');
	assert.equal(exhausted.capacity.accounts.length, 1);
	assert.equal(exhausted.capacity.accounts[0].state, 'exhausted');
	assert.ok(!commands.some((args) => args.includes('run')));
	assert.ok(!JSON.stringify(exhausted).includes('openai-access-secret'));

	// Healthy plan: ready verdict gains capacity; the model probe still runs.
	commands.length = 0;
	const ready = await openCodeProviderReadiness(request, { env, now: NOW, spawnSync, fetch: fetchReturning(fixtures.openai_available) });
	assert.equal(ready.classification, 'ready');
	assert.equal(ready.capacity.scope, 'opencode:openai');
	const { accounts: readyAccounts, ...readySummary } = ready.capacity;
	assert.deepEqual(readySummary, { scope: 'opencode:openai', remaining: 88, limit: 100, unit: 'percent', reset_at: '2026-09-28T23:29:35.000Z' });
	assert.deepEqual(readyAccounts.map((account) => [account.state, account.remaining]), [['available', 88]]);
	assert.ok(commands.some((args) => args.includes('run')));

	// Failed lookup: readiness is unchanged and the diagnostic is attached.
	const degraded = await openCodeProviderReadiness(request, { env, now: NOW, spawnSync, fetch: fetchReturning({}, 503) });
	assert.equal(degraded.classification, 'ready');
	assert.deepEqual(Object.keys(degraded.capacity), ['scope', 'accounts'], 'no route summary when no account was measured');
	assert.equal(degraded.capacity.scope, 'opencode:openai');
	assert.equal(degraded.capacity.accounts[0].state, 'lookup_failed');
	assert.match(degraded.capacity_diagnostic, /could not be read/);
}

async function capacityOnlyReadiness() {
	const env = singleAccountEnv;
	// A capacity-only lookup never needs the OpenCode binary: point runtime_bin
	// at a path that does not exist to prove no executable resolution happens.
	const request = {
		schema: 'homeboy/agent-task-provider-readiness-request/v1',
		effective_config: { runtime_bin: path.join(env.HOME, 'missing-opencode-bin'), model: 'openai/gpt-5.6-terra' },
	};
	let spawns = 0;
	const spawnSync = () => {
		spawns += 1;
		return { status: 0, stdout: '', stderr: '' };
	};

	// Healthy pool: ready from the usage lookup alone, with pool scope attached.
	const ready = await openCodeProviderReadiness({ ...request, mode: 'capacity' }, { env, now: NOW, spawnSync, fetch: fetchReturning(fixtures.openai_available) });
	assert.equal(ready.ready, true);
	assert.equal(ready.classification, 'ready');
	assert.equal(ready.reason, 'capacity_available');
	assert.equal(ready.retryable, false);
	assert.equal(ready.identity.mode, 'capacity');
	assert.equal(ready.capacity.scope, 'opencode:openai');
	assert.equal(ready.capacity.remaining, 88);
	assert.equal(ready.capacity.accounts.length, 1);
	assert.equal(spawns, 0, 'capacity-only readiness spawns no OpenCode process');

	// Exhausted pool: capacity verdict with the reset time, still without a probe.
	const exhausted = await openCodeProviderReadiness({ ...request, mode: 'capacity' }, { env, now: NOW, spawnSync, fetch: fetchReturning(fixtures.openai_exhausted) });
	assert.equal(exhausted.ready, false);
	assert.equal(exhausted.classification, 'capacity');
	assert.equal(exhausted.reason, 'provider_capacity_exhausted');
	assert.equal(exhausted.retryable, true);
	assert.equal(exhausted.capacity.scope, 'opencode:openai');
	assert.equal(exhausted.capacity.reset_at, '2026-09-28T23:29:35.000Z');
	assert.equal(spawns, 0);

	// No published usage endpoint (or no credential): ready with capacity omitted.
	const unpublished = await openCodeProviderReadiness({
		schema: 'homeboy/agent-task-provider-readiness-request/v1',
		effective_config: { runtime_bin: request.effective_config.runtime_bin, model: 'google/gemini-3.1-pro' },
		mode: 'capacity',
	}, { env, now: NOW, spawnSync, fetch: fetchReturning(fixtures.openai_available) });
	assert.equal(unpublished.ready, true);
	assert.equal(unpublished.classification, 'ready');
	assert.equal(unpublished.reason, 'capacity_not_published');
	assert.equal(unpublished.capacity, undefined);
	assert.equal(unpublished.capacity_diagnostic, undefined);
	assert.equal(spawns, 0);

	// An exhausted opencode-go plan: capacity verdict from the usage endpoint
	// alone, still without spawning OpenCode.
	const goExhausted = await openCodeProviderReadiness({
		schema: 'homeboy/agent-task-provider-readiness-request/v1',
		effective_config: { runtime_bin: request.effective_config.runtime_bin, model: 'opencode-go/kimi-k2.7-code' },
		mode: 'capacity',
	}, { env, now: NOW, spawnSync, fetch: fetchReturning(fixtures.opencode_go) });
	assert.equal(goExhausted.ready, false);
	assert.equal(goExhausted.classification, 'capacity');
	assert.equal(goExhausted.reason, 'provider_capacity_exhausted');
	assert.equal(goExhausted.retryable, true);
	assert.equal(goExhausted.capacity.scope, 'opencode:opencode-go');
	assert.equal(goExhausted.capacity.reset_at, '2026-10-08T00:46:00.000Z');
	assert.equal(spawns, 0);

	// Nothing could be measured: ready with accounts-only capacity and a diagnostic.
	const degraded = await openCodeProviderReadiness({ ...request, mode: 'capacity' }, { env, now: NOW, spawnSync, fetch: fetchReturning({}, 503) });
	assert.equal(degraded.ready, true);
	assert.equal(degraded.classification, 'ready');
	assert.equal(degraded.reason, 'capacity_unmeasured');
	assert.deepEqual(Object.keys(degraded.capacity), ['scope', 'accounts']);
	assert.equal(degraded.capacity.accounts[0].state, 'lookup_failed');
	assert.match(degraded.capacity_diagnostic, /could not be read/);
	assert.equal(spawns, 0);

	// Routes on the same provider share the pool scope regardless of model.
	const otherModel = await openCodeProviderReadiness({
		schema: request.schema,
		effective_config: { ...request.effective_config, model: 'openai/gpt-5.6-terra-fast' },
		mode: 'capacity',
	}, { env, now: NOW, spawnSync, fetch: fetchReturning(fixtures.openai_available) });
	assert.equal(otherModel.capacity.scope, 'opencode:openai');
	assert.equal(ready.capacity.scope, otherModel.capacity.scope);

	// Invalid requests keep their configuration verdicts, mode-scoped, without probes.
	const invalidSchema = await openCodeProviderReadiness({ mode: 'capacity', effective_config: request.effective_config }, { env, now: NOW, spawnSync, fetch: fetchReturning(fixtures.openai_available) });
	assert.equal(invalidSchema.classification, 'configuration_failure');
	assert.equal(invalidSchema.reason, 'invalid_readiness_request');
	assert.equal(invalidSchema.identity.mode, 'capacity');
	const invalidRoute = await openCodeProviderReadiness({ ...request, mode: 'capacity', effective_config: { model: 'not-a-route' } }, { env, now: NOW, spawnSync, fetch: fetchReturning(fixtures.openai_available) });
	assert.equal(invalidRoute.classification, 'configuration_failure');
	assert.equal(invalidRoute.reason, 'invalid_provider_model');
	assert.equal(invalidRoute.identity.mode, 'capacity');
	assert.equal(spawns, 0);

	// The capacity-only cache key never collides with a live-inference verdict.
	const executable = path.join(env.HOME, 'opencode-bin');
	fs.writeFileSync(executable, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
	const liveRequest = { schema: request.schema, effective_config: { ...request.effective_config, runtime_bin: executable } };
	const probeCommands = [];
	const probeSpawnSync = (command, args) => {
		probeCommands.push(args);
		if (args.includes('--version')) return { status: 0, stdout: '1.0.0\n', stderr: '' };
		if (args.includes('auth')) return { status: 0, stdout: 'OpenAI oauth\n', stderr: '' };
		if (args.includes('models')) return { status: 0, stdout: 'openai/gpt-5.6-terra\n', stderr: '' };
		return { status: 0, stdout: '{"type":"text","text":"READY"}\n', stderr: '' };
	};
	const full = await openCodeProviderReadiness(liveRequest, { env, now: NOW, spawnSync: probeSpawnSync, fetch: fetchReturning(fixtures.openai_available) });
	assert.equal(full.classification, 'ready');
	assert.equal(full.reason, 'model_execution_ready');
	assert.equal(full.identity.mode, undefined);
	assert.notEqual(full.cache_key, ready.cache_key, 'capacity-only results are never reused as live-inference verdicts');
	assert.ok(probeCommands.some((args) => args.includes('run')), 'a missing or other mode keeps the full probe path');
	const otherMode = await openCodeProviderReadiness({ ...liveRequest, mode: 'probe' }, { env, now: NOW, spawnSync: probeSpawnSync, fetch: fetchReturning(fixtures.openai_available) });
	assert.equal(otherMode.reason, 'model_execution_ready');
	assert.equal(probeCommands.filter((args) => args.includes('--version')).length, 2);
}

(async () => {
	await singleAccountTests();
	await opencodeGoAndXaiTests();
	await poolTests();
	await readinessIntegration();
	await capacityOnlyReadiness();
	console.log('opencode provider capacity tests passed');
})().catch((error) => {
	console.error(error);
	process.exit(1);
});
