'use strict';

require('../../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { piProviderReadiness, piRuntimeReadiness } = require('..');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-pi-readiness-'));
const executable = path.join(root, 'pi');
fs.writeFileSync(executable, '#!/bin/sh\nexit 0\n', { mode: 0o755 });

function request(config = {}) {
	return {
		schema: 'homeboy/agent-task-provider-readiness-request/v1',
		effective_config: { command: executable, model: 'anthropic/claude-test', ...config },
	};
}

const probe = (result) => (command, args) => args[0] === '--list-models'
	? { status: 0, stdout: 'provider  model  context\nanthropic  claude-test  200000\nopenai  gpt-x  128000\n', stderr: '' }
	: result;

try {
	const ready = piRuntimeReadiness(request(), { env: { PATH: process.env.PATH }, spawnSync: probe({ status: 0, stdout: '0.70.1\n', stderr: '' }) });
	assert.equal(ready.ready, true);
	assert.equal(ready.classification, 'ready');
	assert.equal(ready.identity.version, '0.70.1');
	assert.equal(ready.identity.model, 'anthropic/claude-test');
	assert.equal(ready.identity.agent_dir, path.join(os.homedir(), '.pi', 'agent'));
	const selectedEnv = { PATH: process.env.PATH, HOME: '/runner/home', PI_CODING_AGENT_DIR: '/ambient/pi' };
	const seenProbeEnvs = [];
	const selected = piRuntimeReadiness(request({ pi_agent_dir: '~/x' }), {
		env: selectedEnv,
		spawnSync: (command, args, options) => {
			seenProbeEnvs.push(options.env.PI_CODING_AGENT_DIR);
			return args[0] === '--list-models'
				? { status: 0, stdout: 'provider model context\nanthropic claude-test 200000\n', stderr: '' }
				: { status: 0, stdout: '0.70.1\n', stderr: '' };
		},
	});
	assert.equal(selected.identity.agent_dir, path.join('/runner/home', 'x'));
	assert.deepEqual(seenProbeEnvs, [path.join('/runner/home', 'x'), path.join('/runner/home', 'x')]);
	const ambient = piRuntimeReadiness(request(), { env: selectedEnv, spawnSync: (command, args) => args[0] === '--list-models'
		? { status: 0, stdout: 'provider model context\nanthropic claude-test 200000\n', stderr: '' }
		: { status: 0, stdout: '0.70.1\n', stderr: '' } });
	assert.equal(ambient.identity.agent_dir, '/ambient/pi');

	const catalogProbe = (stdout, listResult = {}) => (command, args, options) => args[0] === '--list-models'
		? { status: 0, stdout, stderr: '', ...listResult }
		: { status: 0, stdout: '0.70.1\n', stderr: '' };
	const noModels = piRuntimeReadiness(request(), { env: { PATH: process.env.PATH }, spawnSync: catalogProbe('No models available\n') });
	assert.equal(noModels.classification, 'auth_failure');
	assert.equal(noModels.reason, 'no_models_available');
	const unavailable = piRuntimeReadiness(request(), { env: { PATH: process.env.PATH }, spawnSync: catalogProbe('provider model context\nanthropic other-model 200000\n') });
	assert.equal(unavailable.classification, 'deterministic_incompatibility');
	assert.equal(unavailable.reason, 'model_not_available');
	// A bare model names no provider: Pi would resolve it outside this catalog.
	const bare = piRuntimeReadiness(request({ model: 'opus-5-5' }), { env: { PATH: process.env.PATH }, spawnSync: catalogProbe('provider model context\nroadie anthropic-claude-opus-5-5 200000\nroadie anthropic-claude-sonnet-5-5 200000\nroadie opus-5-5 200000\n') });
	assert.equal(bare.ready, false);
	assert.equal(bare.classification, 'deterministic_incompatibility');
	assert.equal(bare.reason, 'model_not_qualified');
	assert.deepEqual(bare.candidates, ['roadie/anthropic-claude-opus-5-5', 'roadie/opus-5-5']);
	assert.match(bare.remediation, /for example roadie\/anthropic-claude-opus-5-5, roadie\/opus-5-5\.$/);
	const bareUnknown = piRuntimeReadiness(request({ model: 'nothing-like-it' }), { env: { PATH: process.env.PATH }, spawnSync: catalogProbe('provider model context\nroadie cook-heavy 200000\n') });
	assert.equal(bareUnknown.reason, 'model_not_qualified');
	assert.equal(bareUnknown.candidates, undefined);
	const qualified = piRuntimeReadiness(request({ model: 'roadie/cook-heavy' }), { env: { PATH: process.env.PATH }, spawnSync: catalogProbe('provider model context\nroadie cook-heavy 200000\n') });
	assert.equal(qualified.ready, true);
	const timeout = piRuntimeReadiness(request(), { env: { PATH: process.env.PATH }, spawnSync: catalogProbe('', { error: Object.assign(new Error('timed out'), { code: 'ETIMEDOUT' }) }) });
	assert.equal(timeout.classification, 'transient_failure');
	const unspecified = piRuntimeReadiness(request({ model: undefined }), { env: { PATH: process.env.PATH }, spawnSync: catalogProbe('provider model context\nany model 1000\n') });
	assert.equal(unspecified.ready, true);

	// Not installed: not ready (the old runtime reported structurally dispatchable here).
	const missing = piRuntimeReadiness(request({ command: path.join(root, 'missing-pi') }), { env: { PATH: '' } });
	assert.equal(missing.ready, false);
	assert.equal(missing.classification, 'deterministic_incompatibility');

	// Auth rejection from the version probe.
	const unauthorized = piRuntimeReadiness(request(), { env: { PATH: process.env.PATH }, spawnSync: probe({ status: 1, stdout: '', stderr: 'authentication failed' }) });
	assert.equal(unauthorized.ready, false);
	assert.equal(unauthorized.classification, 'auth_failure');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

// Capacity (homeboy-extensions#2940): pi's own auth store, account identity
// from the profile endpoint, usage windows from the plan usage endpoint.
(async () => {
	const agentDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-pi-capacity-'));
	const bin = path.join(agentDir, 'pi');
	fs.writeFileSync(bin, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
	const now = Date.parse('2026-10-13T15:00:00Z');
	fs.writeFileSync(path.join(agentDir, 'auth.json'), JSON.stringify({
		anthropic: { type: 'oauth', access: 'access-token', refresh: 'refresh-token', expires: now + 3_600_000 },
	}));
	const env = { PATH: process.env.PATH, PI_CODING_AGENT_DIR: path.join(agentDir, 'ambient') };
	const capacityRequest = (extra = {}) => ({
		schema: 'homeboy/agent-task-provider-readiness-request/v1',
		effective_config: { command: bin, model: 'anthropic/claude-test', pi_agent_dir: agentDir },
		...extra,
	});
	const usage = (utilization) => async (url, init) => {
		assert.equal(init.headers.Authorization, 'Bearer access-token');
		if (url.endsWith('/profile')) {
			return { ok: true, json: async () => ({ account: { email_address: 'owner@example.test' } }) };
		}
		return { ok: true, json: async () => ({
			five_hour: { utilization, resets_at: '2026-10-13T18:00:00Z' },
			extra_usage: { is_enabled: extraEnabled, spend_limit_reached: false, user_disabled: !extraEnabled },
		}) };
	};
	let extraEnabled = true;
	const spawn = probe({ status: 0, stdout: '0.70.1\n', stderr: '' });
	try {
		const available = await piProviderReadiness(capacityRequest(), { env, now, fetch: usage(40), spawnSync: spawn });
		assert.equal(available.ready, true);
		assert.equal(available.capacity.scope, 'pi:anthropic');
		assert.equal(available.capacity.remaining, 60);
		assert.equal(available.capacity.accounts[0].account, 'owner@example.test');
		assert.equal(JSON.stringify(available).includes('access-token'), false);

		const exhausted = await piProviderReadiness(capacityRequest(), { env, now, fetch: usage(100), spawnSync: spawn });
		assert.equal(exhausted.ready, false);
		assert.equal(exhausted.classification, 'capacity');
		assert.equal(exhausted.capacity.reset_at, '2026-10-13T18:00:00.000Z');

		const capacityOnly = await piProviderReadiness(capacityRequest({ mode: 'capacity' }), {
			env,
			now,
			fetch: usage(40),
			spawnSync: () => { throw new Error('capacity mode must not spawn pi'); },
		});
		assert.equal(capacityOnly.reason, 'capacity_available');
		assert.equal(capacityOnly.identity.mode, 'capacity');
		assert.notEqual(capacityOnly.cache_key, available.cache_key);

		// Plan headroom but extra usage disabled: pi cannot run on this login.
		extraEnabled = false;
		const noExtra = await piProviderReadiness(capacityRequest(), { env, now, fetch: usage(10), spawnSync: spawn });
		assert.equal(noExtra.classification, 'capacity');
		assert.equal(noExtra.capacity.accounts[0].extra_usage.enabled, false);
		assert.match(noExtra.capacity.accounts[0].diagnostic, /extra usage is disabled/);
		extraEnabled = true;

		// No stored login for the provider: no capacity, verdict unchanged.
		const none = await piProviderReadiness(capacityRequest({ effective_config: { command: bin, model: 'openai/gpt-x' } }), { env, now, fetch: usage(40), spawnSync: spawn });
		assert.equal(none.ready, true);
		assert.equal(none.capacity, undefined);
	} finally {
		fs.rmSync(agentDir, { recursive: true, force: true });
	}
	process.stdout.write('Pi runtime readiness passed\n');
})().catch((error) => {
	console.error(error);
	process.exitCode = 1;
});
