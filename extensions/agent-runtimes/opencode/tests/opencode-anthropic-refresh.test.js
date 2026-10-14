'use strict';

// CI has no homeboy binary; pin the runtime contract constants first.
require('../../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');

// Controller-side Anthropic OAuth refresh (homeboy#15783).

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {
	MIN_REMAINING_MS,
	RefreshError,
	TOKEN_URL,
	ensureFreshAnthropicLogin,
} = require('../lib/opencode-anthropic-refresh');
const {
	ANTHROPIC_OAUTH_ACCOUNT,
	resolveOpenCodeAuthPlan,
	selectedOpenCodeRoute,
} = require('../lib/opencode-auth-plan');
const { providerContract } = require('../lib/opencode-agent-task-executor');

const NOW = Date.parse('2026-10-11T23:00:00Z');
const HOUR = 60 * 60 * 1000;

function fixture({ anthropic, accounts, other = { openai: { type: 'oauth', access: 'runner-openai', refresh: 'ro', expires: NOW + 100 * HOUR } } }) {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), 'opencode-anthropic-refresh-'));
	const dataDir = path.join(root, 'opencode');
	fs.mkdirSync(dataDir, { recursive: true });
	const authPath = path.join(dataDir, 'auth.json');
	const accountsPath = path.join(dataDir, 'anthropic-oauth-accounts.json');
	fs.writeFileSync(authPath, JSON.stringify({ ...other, ...(anthropic ? { anthropic } : {}) }));
	if (accounts) fs.writeFileSync(accountsPath, JSON.stringify(accounts));
	return { env: { XDG_DATA_HOME: root, HOME: root }, authPath, accountsPath };
}

function tokenEndpoint(responses) {
	const calls = [];
	const fetch = async (url, init) => {
		calls.push({ url, body: JSON.parse(init.body) });
		const next = responses.shift();
		return { ok: next.status === 200, status: next.status, text: async () => JSON.stringify(next.body) };
	};
	return { fetch, calls };
}

const read = (file) => JSON.parse(fs.readFileSync(file, 'utf8'));

(async () => {
	// A token with plenty of life is left alone: no request, no rotation.
	{
		const f = fixture({ anthropic: { type: 'oauth', access: 'a', refresh: 'r', expires: NOW + MIN_REMAINING_MS + HOUR } });
		const endpoint = tokenEndpoint([]);
		const result = await ensureFreshAnthropicLogin({ env: f.env, now: () => NOW, fetch: endpoint.fetch });
		assert.equal(result.refreshed, false);
		assert.equal(endpoint.calls.length, 0);
	}

	// A token about to expire is refreshed in auth.json, the matching pool
	// record follows it, and unrelated logins are untouched.
	{
		const f = fixture({
			anthropic: { type: 'oauth', access: 'old-access', refresh: 'old-refresh', expires: NOW + HOUR },
			accounts: { version: 1, activeIndex: 1, accounts: [
				{ type: 'oauth', access: 'dead', refresh: 'dead', expires: 0, addedAt: 1, lastUsed: 1 },
				{ type: 'oauth', access: 'old-access', refresh: 'old-refresh', expires: NOW + HOUR, email: 'me@example.com', addedAt: 2, lastUsed: 2 },
			] },
		});
		const endpoint = tokenEndpoint([{ status: 200, body: { access_token: 'new-access', refresh_token: 'new-refresh', expires_in: 28800 } }]);
		const result = await ensureFreshAnthropicLogin({ env: f.env, now: () => NOW, fetch: endpoint.fetch });

		assert.equal(result.refreshed, true);
		assert.equal(endpoint.calls[0].url, TOKEN_URL);
		assert.equal(endpoint.calls[0].body.grant_type, 'refresh_token');
		assert.equal(endpoint.calls[0].body.refresh_token, 'old-refresh');
		const auth = read(f.authPath);
		assert.equal(auth.anthropic.access, 'new-access');
		assert.equal(auth.anthropic.refresh, 'new-refresh');
		assert.equal(auth.anthropic.expires, NOW + 28800 * 1000 - 5 * 60 * 1000);
		assert.equal(auth.openai.access, 'runner-openai');
		const accounts = read(f.accountsPath);
		assert.equal(accounts.accounts[1].refresh, 'new-refresh');
		assert.equal(accounts.accounts[1].email, 'me@example.com');
		assert.equal(accounts.accounts[0].refresh, 'dead');
		assert.equal(fs.statSync(f.authPath).mode & 0o777, 0o600);
		assert.equal(fs.existsSync(`${f.authPath}.lock`), false);
	}

	// A dead refresh token fails with an actionable message and no token text.
	{
		const f = fixture({ anthropic: { type: 'oauth', access: 'old', refresh: 'secret-refresh', expires: NOW - HOUR } });
		const endpoint = tokenEndpoint([{ status: 400, body: { error: 'invalid_grant', error_description: 'Refresh token not found or invalid' } }]);
		await assert.rejects(
			ensureFreshAnthropicLogin({ env: f.env, now: () => NOW, fetch: endpoint.fetch }),
			(error) => error instanceof RefreshError
				&& /invalid_grant/.test(error.message)
				&& /opencode auth login/.test(error.message)
				&& !error.message.includes('secret-refresh'),
		);
		assert.equal(read(f.authPath).anthropic.refresh, 'secret-refresh');
		assert.equal(fs.existsSync(`${f.authPath}.lock`), false);
	}

	// No OAuth login at all (API key or nothing) is an operator error, not a crash.
	{
		const f = fixture({ anthropic: { type: 'api', key: 'k' } });
		await assert.rejects(
			ensureFreshAnthropicLogin({ env: f.env, now: () => NOW, fetch: tokenEndpoint([]).fetch }),
			/No Anthropic OAuth login/,
		);
	}

	// A held lock that goes stale is reclaimed; the re-read under the lock
	// sees a token another process already refreshed and skips the request.
	{
		const f = fixture({ anthropic: { type: 'oauth', access: 'old', refresh: 'r', expires: NOW } });
		fs.mkdirSync(`${f.authPath}.lock`);
		let clock = NOW;
		const sleep = async () => {
			clock += 31_000;
			const auth = read(f.authPath);
			auth.anthropic = { type: 'oauth', access: 'fresh', refresh: 'r2', expires: clock + 8 * HOUR };
			fs.writeFileSync(f.authPath, JSON.stringify(auth));
			fs.utimesSync(`${f.authPath}.lock`, new Date(NOW), new Date(NOW));
		};
		const endpoint = tokenEndpoint([]);
		const result = await ensureFreshAnthropicLogin({ env: f.env, now: () => clock, fetch: endpoint.fetch, sleep });
		assert.equal(result.refreshed, false);
		assert.equal(endpoint.calls.length, 0);
	}

	// `anthropic-oauth` is an explicit selector for the anthropic route, and its
	// store handoff never declares the refresh token.
	{
		assert.deepEqual(
			selectedOpenCodeRoute({ provider: ANTHROPIC_OAUTH_ACCOUNT, model: 'anthropic/claude-sonnet-5-5' }),
			{ provider: 'anthropic', model: 'claude-sonnet-5-5' },
		);
		const f = fixture({ anthropic: { type: 'oauth', access: 'a', refresh: 'r', expires: NOW + 8 * HOUR } });
		const plan = resolveOpenCodeAuthPlan(
			{ provider: ANTHROPIC_OAUTH_ACCOUNT, model: 'anthropic/claude-sonnet-5-5' },
			{ env: f.env },
		);
		assert.deepEqual(plan.secret_env, ['AI_PROVIDER_OPENCODE_ANTHROPIC_ACCESS', 'AI_PROVIDER_OPENCODE_ANTHROPIC_EXPIRES']);
		assert.deepEqual(Object.keys(plan.secret_env_sources).sort(), plan.secret_env.slice().sort());
		assert.equal(plan.secret_env_sources.AI_PROVIDER_OPENCODE_ANTHROPIC_ACCESS.field, 'anthropic.access');

		const account = providerContract().provider_defaults[ANTHROPIC_OAUTH_ACCOUNT];
		assert.deepEqual(account.model_providers, ['anthropic']);
		assert.deepEqual(account.secret_env, plan.secret_env);
		assert.equal(Object.values(account.secret_env_sources).some((source) => source.field === 'anthropic.refresh'), false);
		assert.deepEqual(account.controller_refresh.argv, ['node', '{{runtime_path}}/scripts/agent/homeboy-opencode-anthropic-refresh.cjs']);
		assert.ok(fs.existsSync(path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-opencode-anthropic-refresh.cjs')));
		// Plain Anthropic OAuth routes resolve to the same controller-owned
		// credential account; there is no second runner-owned OAuth path.
		const plainPlan = resolveOpenCodeAuthPlan(
			{ model: 'anthropic/claude-sonnet-5-5' },
			{ env: f.env },
		);
		assert.equal(plainPlan.account_kind, ANTHROPIC_OAUTH_ACCOUNT);
		assert.deepEqual(plainPlan.secret_env, plan.secret_env);
		assert.deepEqual(plainPlan.secret_env_sources, plan.secret_env_sources);
		assert.equal(providerContract().provider_defaults[ANTHROPIC_OAUTH_ACCOUNT].controller_refresh.argv[0], 'node');

		// An expired runner-side access-only handoff is an explicit operator
		// action, not a generic provider authentication rejection.
		const expired = fixture({ anthropic: { type: 'oauth', access: 'expired', expires: NOW - HOUR } });
		const expiredPlan = resolveOpenCodeAuthPlan({ model: 'anthropic/claude-sonnet-5-5' }, { env: expired.env });
		const { openCodeAuthPlanError } = require('../lib/opencode-auth-plan');
		assert.match(openCodeAuthPlanError(expiredPlan, expired.env), /re-login Anthropic on the controller/);
		const refreshable = fixture({ anthropic: { type: 'oauth', access: 'expired', refresh: 'still-valid', expires: NOW - HOUR } });
		const refreshablePlan = resolveOpenCodeAuthPlan({ model: 'anthropic/claude-sonnet-5-5' }, { env: refreshable.env });
		assert.equal(openCodeAuthPlanError(refreshablePlan, refreshable.env), null);
	}

	process.stdout.write('opencode anthropic refresh tests passed\n');
})().catch((error) => {
	process.stderr.write(`${error.stack}\n`);
	process.exitCode = 1;
});
