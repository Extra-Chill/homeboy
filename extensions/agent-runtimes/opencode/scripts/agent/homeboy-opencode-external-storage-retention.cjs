#!/usr/bin/env node
'use strict';

const { handleRequest } = require('../../lib/opencode-external-storage-retention');

try {
	const input = JSON.parse(require('node:fs').readFileSync(0, 'utf8'));
	process.stdout.write(`${JSON.stringify(handleRequest(input))}\n`);
} catch (error) {
	process.stderr.write(`OpenCode external storage retention: ${error.message}\n`);
	process.exitCode = 1;
}
