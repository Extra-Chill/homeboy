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
});

// Responds per credential so pooled accounts can carry different usage.
function fetchByCredential(responses, calls = []) {
	return async (url, init) => {
		calls.push({ url, init });
		const credential = String(init.headers.Authorization).replace(/^Bearer /, '');
		const [status, body] = responses[credential] || [500, {}];
		return { ok: status >= 200 && status < 300, status, json: async () => body };
	};
}

function fetchReturning(body, status = 200, calls = []) {
	return async (url, init) => {
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
	assert.equal(await openCodeProviderCapacity('opencode-go', options(noCall)), null);
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

async function poolTests() {
	const oauth = (access, extra = {}) => ({ type: 'oauth', access, refresh: `${access}-refresh`, expires: FUTURE, ...extra });
	const weeklySpent = (resetsAt) => ({ five_hour: { utilization: 0, resets_at: null }, seven_day: { utilization: 100, resets_at: resetsAt } });

	// Four Claude plans plus a stale login: three spent for the week, one healthy.
	const env = dataHome({ anthropic: oauth('active') }, {
		anthropic: [
			oauth('stale', { expires: NOW - 1, email: 'old@example.com' }),
			oauth('plan-a'),
			oauth('plan-b'),
			oauth('plan-c'),
			oauth('active'),
		],
		openai: [
			oauth('codex-1', { email: 'one@example.com', accountId: 'acct-1' }),
			oauth('codex-2', { email: 'two@example.com', accountId: 'acct-2' }),
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
		}, calls),
	});
	assert.equal(calls.length, 4, 'expired pooled credentials are reported without a request');
	assert.deepEqual(pool.accounts.map((account) => [account.account, account.state]), [
		['old@example.com', 'credential_expired'],
		['anthropic#1', 'exhausted'],
		['anthropic#2', 'exhausted'],
		['anthropic#3', 'exhausted'],
		['anthropic#4', 'available'],
	]);
	assert.equal(pool.accounts[1].reset_at, '2026-09-25T03:00:00.000Z');
	assert.equal(pool.exhausted, false);
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
	assert.deepEqual(codex.accounts.map((account) => [account.account, account.reset_at]), [
		['one@example.com', '2026-09-27T15:26:10.000Z'],
		['two@example.com', '2026-09-28T23:29:35.000Z'],
	]);
	assert.deepEqual(codex.capacity, { remaining: 0, limit: 100, unit: 'percent', reset_at: '2026-09-27T15:26:10.000Z' });

	// An unmeasurable account keeps an otherwise spent pool from being declared exhausted.
	const partial = await openCodeProviderCapacity('openai', {
		env,
		now: NOW,
		fetch: fetchByCredential({ 'codex-1': [200, fixtures.openai_exhausted], 'codex-2': [503, {}] }),
	});
	assert.equal(partial.exhausted, false);
	assert.equal(partial.capacity.remaining, 0);
	assert.match(partial.diagnostic, /1 of 2 openai account/);
	assert.ok(!JSON.stringify([pool, codex, partial]).match(/codex-1|plan-a|refresh/));
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
	assert.equal(exhausted.capacity.reset_at, '2026-09-28T23:29:35.000Z');
	assert.equal(exhausted.capacity_accounts.length, 1);
	assert.ok(!commands.some((args) => args.includes('run')));
	assert.ok(!JSON.stringify(exhausted).includes('openai-access-secret'));

	// Healthy plan: ready verdict gains capacity; the model probe still runs.
	commands.length = 0;
	const ready = await openCodeProviderReadiness(request, { env, now: NOW, spawnSync, fetch: fetchReturning(fixtures.openai_available) });
	assert.equal(ready.classification, 'ready');
	assert.deepEqual(ready.capacity, { remaining: 88, limit: 100, unit: 'percent', reset_at: '2026-09-28T23:29:35.000Z' });
	assert.ok(commands.some((args) => args.includes('run')));

	// Failed lookup: readiness is unchanged and the diagnostic is attached.
	const degraded = await openCodeProviderReadiness(request, { env, now: NOW, spawnSync, fetch: fetchReturning({}, 503) });
	assert.equal(degraded.classification, 'ready');
	assert.equal(degraded.capacity, undefined);
	assert.match(degraded.capacity_diagnostic, /could not be read/);
}

(async () => {
	await singleAccountTests();
	await poolTests();
	await readinessIntegration();
	console.log('opencode provider capacity tests passed');
})().catch((error) => {
	console.error(error);
	process.exit(1);
});
