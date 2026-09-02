'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { openCodeRuntimeReadiness } = require('..');
const fixtures = require('./fixtures/provider-readiness.json');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-opencode-readiness-'));
const executable = path.join(root, 'opencode');
fs.writeFileSync(executable, '#!/bin/sh\nexit 0\n', { mode: 0o755 });

function request(config = {}) {
	return {
		schema: 'homeboy/agent-task-provider-readiness-request/v1',
		effective_config: { runtime_bin: executable, model: 'openai/gpt-5.6-terra', ...config },
	};
}

function env(extra = {}) {
	return { PATH: process.env.PATH, HOME: root, OPENAI_API_KEY: 'secret-do-not-leak', ...extra };
}

function probe(responses, expectedTimeout = 15_000) {
	let index = 0;
	return (command, args, options) => {
		assert.equal(command, executable);
		assert.equal(options.timeout, expectedTimeout);
		assert.equal(options.maxBuffer, 16 * 1024);
		const response = responses[index++];
		assert.deepEqual(args.slice(-response.args.length), response.args);
		if (response.args[0] === 'run') {
			assert.ok(options.cwd);
			assert.equal(JSON.parse(options.env.OPENCODE_CONFIG_CONTENT).agent['homeboy-readiness'].permission['*'], 'deny');
		}
		return response.result;
	};
}

function readyResponses(providerResult = fixtures.success, model = 'gpt-5.6-terra') {
	return [
		{ args: ['--version'], result: { status: 0, stdout: '1.18.25\n', stderr: '' } },
		{ args: ['auth', 'list'], result: { status: 0, stdout: 'Credentials ~/.local/share/opencode/auth.json\nOpenAI oauth\n', stderr: '' } },
		{ args: ['models', 'openai'], result: { status: 0, stdout: `openai/${model}\n`, stderr: '' } },
		{ args: ['run', '--model', `openai/${model}`, '--format', 'json', '--agent', 'homeboy-readiness', '--title', 'homeboy-readiness', 'Reply with exactly READY. Do not access files, run commands, or make changes.'], result: providerResult },
	];
}

try {
	const ready = openCodeRuntimeReadiness(request({ provider: 'codex' }), { env: env(), spawnSync: probe(readyResponses()) });
	assert.equal(ready.schema, 'homeboy/agent-task-provider-readiness-result/v1');
	assert.equal(ready.ready, true);
	assert.equal(ready.classification, 'ready');
	assert.equal(ready.identity.provider, 'openai');
	assert.equal(ready.identity.model, 'gpt-5.6-terra');
	assert.equal(ready.identity.version, '1.18.25');
	assert.equal(ready.reason, 'model_execution_ready');
	assert.equal(JSON.stringify(ready).includes('secret-do-not-leak'), false);
	const clampedTimeout = openCodeRuntimeReadiness(request({ readiness_timeout_ms: 60_000 }), {
		env: env(),
		spawnSync: probe(readyResponses(), 30_000),
	});
	assert.equal(clampedTimeout.ready, true);
	const changedModel = openCodeRuntimeReadiness(request({ model: 'openai/gpt-5.6-terra-fast' }), { env: env(), spawnSync: probe(readyResponses(fixtures.success, 'gpt-5.6-terra-fast')) });
	assert.notEqual(changedModel.cache_key, ready.cache_key);
	const changedCredential = openCodeRuntimeReadiness(request(), { env: env({ OPENAI_API_KEY: 'other-secret' }), spawnSync: probe(readyResponses()) });
	assert.notEqual(changedCredential.cache_key, ready.cache_key);
	const authStore = path.join(root, '.local', 'share', 'opencode');
	fs.mkdirSync(authStore, { recursive: true });
	fs.writeFileSync(path.join(authStore, 'auth.json'), '{"openai":"first"}');
	const firstAuthStore = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe(readyResponses()) });
	fs.writeFileSync(path.join(authStore, 'auth.json'), '{"openai":"second"}');
	const secondAuthStore = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe(readyResponses()) });
	assert.notEqual(firstAuthStore.cache_key, secondAuthStore.cache_key);

	const missingAuth = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe([
		readyResponses()[0],
		{ args: ['auth', 'list'], result: { status: 0, stdout: 'Anthropic oauth\n', stderr: '' } },
	]) });
	assert.equal(missingAuth.classification, 'auth_failure');
	assert.equal(missingAuth.reason, 'provider_credentials_missing');
	const rejectedAuth = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe([
		readyResponses()[0],
		{ args: ['auth', 'list'], result: { status: 1, stdout: '', stderr: 'authentication failed for secret-do-not-leak' } },
	]) });
	assert.equal(rejectedAuth.classification, 'auth_failure');
	assert.equal(JSON.stringify(rejectedAuth).includes('secret-do-not-leak'), false);
	const accountBlocked = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe(readyResponses(fixtures.account_blocked)) });
	assert.equal(accountBlocked.ready, false);
	assert.equal(accountBlocked.classification, 'provider_account_blocked');
	assert.equal(accountBlocked.reason, 'provider_account_blocked');
	const quota = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe(readyResponses(fixtures.quota)) });
	assert.equal(quota.classification, 'provider_quota');
	assert.equal(quota.retryable, true);
	const providerAuth = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe(readyResponses(fixtures.auth)) });
	assert.equal(providerAuth.classification, 'auth_failure');
	const providerTransient = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe(readyResponses(fixtures.transient)) });
	assert.equal(providerTransient.classification, 'transient_failure');
	const unsupported = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe(readyResponses(fixtures.unsupported)) });
	assert.equal(unsupported.classification, 'configuration_failure');
	assert.equal(unsupported.reason, 'model_unsupported');
	const indeterminate = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe(readyResponses(fixtures.indeterminate)) });
	assert.equal(indeterminate.ready, false);
	assert.equal(indeterminate.classification, 'indeterminate');
	assert.equal(indeterminate.reason, 'model_probe_unsupported');
	const transient = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe([
		{ args: ['--version'], result: { error: { code: 'ETIMEDOUT' }, stdout: '', stderr: '' } },
	]) });
	assert.equal(transient.classification, 'transient_failure');
	assert.equal(transient.retryable, true);
	const outputLimit = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe([
		{ args: ['--version'], result: { error: { code: 'ENOBUFS' }, stdout: '', stderr: '' } },
	]) });
	assert.equal(outputLimit.classification, 'configuration_failure');
	assert.equal(outputLimit.reason, 'version_output_limit');
	const configuration = openCodeRuntimeReadiness(request({ model: 'not-a-route' }), { env: env() });
	assert.equal(configuration.classification, 'configuration_failure');
	assert.equal(configuration.reason, 'invalid_provider_model');
	const unavailableModel = openCodeRuntimeReadiness(request(), { env: env(), spawnSync: probe([
		readyResponses()[0], readyResponses()[1],
		{ args: ['models', 'openai'], result: { status: 0, stdout: 'openai/other\n', stderr: '' } },
	]) });
	assert.equal(unavailableModel.classification, 'configuration_failure');
	assert.equal(unavailableModel.reason, 'model_not_available');

	const readinessScript = path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-opencode-provider-readiness.cjs');
	const invocation = spawnSync(process.execPath, [readinessScript], {
		encoding: 'utf8',
		env: { ...process.env, ...env() },
		input: JSON.stringify(request({ runtime_bin: process.execPath, model: 'openai/gpt-5.6-terra' })),
	});
	assert.equal(invocation.status, 0, invocation.stderr);
	const invocationResult = JSON.parse(invocation.stdout);
	assert.equal(invocationResult.schema, 'homeboy/agent-task-provider-readiness-result/v1');
	assert.equal(typeof invocationResult.ready, 'boolean');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('OpenCode runtime readiness passed\n');
