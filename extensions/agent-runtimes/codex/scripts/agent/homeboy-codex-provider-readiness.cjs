#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const { codexRuntimeReadiness } = require('../../lib/codex-agent-task-executor');

try {
	const request = JSON.parse(fs.readFileSync(0, 'utf8'));
	process.stdout.write(`${JSON.stringify(codexRuntimeReadiness(request))}\n`);
} catch (error) {
	process.stderr.write(`${error.message}\n`);
	process.exitCode = 1;
}
