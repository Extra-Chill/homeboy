#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');

const {
	runtimeManifest,
} = require('../../lib/opencode-runtime-manifest');

const manifestPath = path.join(__dirname, '..', '..', 'opencode.json');
const packagePath = path.join(__dirname, '..', '..', 'package.json');
const manifestJson = `${JSON.stringify(runtimeManifest(), null, 2)}\n`;
const command = process.argv[2] || '--print';

if (command === '--write') {
	fs.writeFileSync(manifestPath, manifestJson);
} else if (command === '--check') {
	try {
		assert.equal(
			JSON.parse(fs.readFileSync(packagePath, 'utf8')).version,
			runtimeManifest().version,
			'OpenCode package and manifest versions must match so runtime installs refresh executable changes.'
		);
		assert.deepEqual(JSON.parse(fs.readFileSync(manifestPath, 'utf8')), runtimeManifest());
	} catch {
		process.stderr.write('agent-runtimes/opencode/opencode.json is out of date. Run `node scripts/agent/homeboy-opencode-runtime-manifest.cjs --write`.\n');
		process.exit(1);
	}
} else if (command === '--print') {
	process.stdout.write(manifestJson);
} else {
	process.stderr.write(`Unknown command: ${command}\n`);
	process.exit(1);
}
