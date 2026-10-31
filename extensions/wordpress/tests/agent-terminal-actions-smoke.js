'use strict';

/* eslint-disable no-console */

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	executeTerminalAction,
	normalizeTerminalAction,
} = require('../lib/agent-terminal-actions');

const fixtureDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-agent-terminal-'));
const binDir = path.join(fixtureDir, 'bin');
fs.mkdirSync(binDir, { recursive: true });
fs.writeFileSync(
	path.join(binDir, 'wp'),
	'#!/usr/bin/env bash\nprintf "wp:%s\\n" "$*"\n',
	{ mode: 0o755 }
);

async function main() {
	try {
		const normalized = normalizeTerminalAction(
			{ type: 'wp_cli', command: 'option get blogname' },
			{ runtimeRoot: fixtureDir, env: { PATH: `${binDir}:${process.env.PATH || ''}` } }
		);

		assert.equal(normalized.command, 'wp option get blogname');
		assert.equal(normalized.cwd, fixtureDir);

		const result = await executeTerminalAction(
			{ type: 'wp_cli', command: 'option get blogname', timeout_ms: 5000 },
			{ runtimeRoot: fixtureDir, env: { PATH: `${binDir}:${process.env.PATH || ''}` } }
		);

		assert.equal(result.success, true);
		assert.equal(result.exitCode, 0);
		assert.equal(result.command, 'wp option get blogname');
		assert.match(result.stdout, /wp:option get blogname/);
		assert.equal(result.stderr, '');

		assert.throws(
			() => normalizeTerminalAction({ type: 'wp_cli', command: 'option get siteurl', cwd: '../outside' }, { runtimeRoot: fixtureDir }),
			/cwd must stay inside the runtime root/
		);

		console.log('Agent terminal actions smoke passed.');
	} finally {
		fs.rmSync(fixtureDir, { recursive: true, force: true });
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
