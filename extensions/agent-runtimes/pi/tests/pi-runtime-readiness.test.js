'use strict';

require('../../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { piRuntimeReadiness } = require('..');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-pi-readiness-'));
const executable = path.join(root, 'pi');
fs.writeFileSync(executable, '#!/bin/sh\nexit 0\n', { mode: 0o755 });

function request(config = {}) {
	return {
		schema: 'homeboy/agent-task-provider-readiness-request/v1',
		effective_config: { command: executable, model: 'anthropic/claude-test', ...config },
	};
}

const probe = (result) => () => result;

try {
	const ready = piRuntimeReadiness(request(), { env: { PATH: process.env.PATH }, spawnSync: probe({ status: 0, stdout: '0.70.1\n', stderr: '' }) });
	assert.equal(ready.ready, true);
	assert.equal(ready.classification, 'ready');
	assert.equal(ready.identity.version, '0.70.1');
	assert.equal(ready.identity.model, 'anthropic/claude-test');

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

process.stdout.write('Pi runtime readiness passed\n');
