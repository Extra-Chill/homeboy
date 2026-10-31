'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { codexRuntimeReadiness } = require('..');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codex-readiness-'));
const executable = path.join(root, 'codex');
fs.writeFileSync(executable, '#!/bin/sh\nexit 0\n', { mode: 0o755 });

function request(config = {}) {
	return {
		schema: 'homeboy/agent-task-provider-readiness-request/v1',
		effective_config: { command: executable, model: 'requested-model', ...config },
	};
}

function env(extra = {}) {
	return {
		PATH: process.env.PATH,
		AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'access',
		AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'refresh',
		AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '9999999999',
		AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'account',
		...extra,
	};
}

function probe(result) {
	return () => result;
}

try {
	const ready = codexRuntimeReadiness(request(), { env: env(), spawnSync: probe({ status: 0, stdout: 'codex-cli 1.2.3\n', stderr: '' }) });
	assert.equal(ready.ready, true);
	assert.equal(ready.classification, 'ready');
	assert.equal(ready.retryable, false);
	assert.equal(ready.identity.executable, executable);
	assert.equal(ready.identity.version, 'codex-cli 1.2.3');
	assert.equal(ready.identity.model, 'requested-model');
	assert.equal(ready.cache_key, ready.identity.cache_key);
	assert.equal(ready.message, ready.remediation);
	assert.equal(ready.cache_key, codexRuntimeReadiness(request(), { env: env(), spawnSync: probe({ status: 0, stdout: 'codex-cli 1.2.3\n', stderr: '' }) }).cache_key);
	assert.equal(JSON.stringify(ready).includes('access'), false);
	const changedCredential = codexRuntimeReadiness(request(), { env: env({ AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'different-access' }), spawnSync: probe({ status: 0, stdout: 'codex-cli 1.2.3\n', stderr: '' }) });
	assert.notEqual(changedCredential.cache_key, ready.cache_key);
	const changedEnvironment = codexRuntimeReadiness(request(), { env: env({ HOMEBOY_CODEX_COMMAND_ARGS: '["exec","--full-auto"]' }), spawnSync: probe({ status: 0, stdout: 'codex-cli 1.2.3\n', stderr: '' }) });
	assert.notEqual(changedEnvironment.cache_key, ready.cache_key);
	const changedModel = codexRuntimeReadiness(request({ model: 'other-model' }), { env: env(), spawnSync: probe({ status: 0, stdout: 'codex-cli 1.2.3\n', stderr: '' }) });
	assert.notEqual(changedModel.cache_key, ready.cache_key);
	const changedRuntime = codexRuntimeReadiness(request(), { env: env(), spawnSync: probe({ status: 0, stdout: 'codex-cli 1.2.4\n', stderr: '' }) });
	assert.notEqual(changedRuntime.cache_key, ready.cache_key);
	let timeout = 0;
	codexRuntimeReadiness(request({ readiness_timeout_ms: 60_000 }), {
		env: env(),
		spawnSync: (command, args, options) => {
			timeout = options.timeout;
			return { status: 0, stdout: 'codex-cli 1.2.3\n', stderr: '' };
		},
	});
	assert.equal(timeout, 5_000);

	const incompatible = codexRuntimeReadiness(request({ command: path.join(root, 'missing') }), { env: env() });
	assert.equal(incompatible.classification, 'deterministic_incompatibility');
	assert.equal(incompatible.retryable, false);
	assert.equal(incompatible.reason, 'executable_not_found');

	const authFailure = codexRuntimeReadiness(request(), { env: env({ AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: '' }) });
	assert.equal(authFailure.classification, 'auth_failure');
	assert.equal(authFailure.retryable, false);
	assert.deepEqual(authFailure.missing_secret_env, ['AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN']);

	const unknownMetadata = codexRuntimeReadiness(request(), { env: env(), spawnSync: probe({ status: 0, stdout: '', stderr: '' }) });
	assert.equal(unknownMetadata.classification, 'unknown_metadata');
	assert.equal(unknownMetadata.retryable, false);
	const rejectedVersion = codexRuntimeReadiness(request(), { env: env(), spawnSync: probe({ status: 1, stdout: '', stderr: '' }) });
	assert.equal(rejectedVersion.classification, 'deterministic_incompatibility');
	assert.equal(rejectedVersion.cache_key, unknownMetadata.cache_key);

	const transient = codexRuntimeReadiness(request(), { env: env(), spawnSync: probe({ error: { code: 'ETIMEDOUT' }, stdout: '', stderr: '' }) });
	assert.equal(transient.classification, 'transient_failure');
	assert.equal(transient.retryable, true);
	const interrupted = codexRuntimeReadiness(request(), { env: env(), spawnSync: probe({ signal: 'SIGTERM', stdout: '', stderr: '' }) });
	assert.equal(interrupted.classification, 'transient_failure');
	assert.equal(interrupted.reason, 'version_probe_interrupted');

	const rejectedAuth = codexRuntimeReadiness(request(), { env: env(), spawnSync: probe({ status: 1, stdout: '', stderr: 'authentication failed' }) });
	assert.equal(rejectedAuth.classification, 'auth_failure');
	assert.equal(rejectedAuth.reason, 'authentication_rejected');

	const readinessScript = path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-codex-provider-readiness.cjs');
	const invocation = spawnSync(process.execPath, [readinessScript], {
		encoding: 'utf8',
		env: { ...process.env, ...env() },
		input: JSON.stringify(request({ command: process.execPath })),
	});
	assert.equal(invocation.status, 0, invocation.stderr);
	const invocationResult = JSON.parse(invocation.stdout);
	assert.equal(invocationResult.schema, 'homeboy/agent-task-provider-readiness-result/v1');
	assert.equal(invocationResult.classification, 'ready');
	assert.equal(typeof invocationResult.message, 'string');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write('Codex runtime readiness passed\n');
