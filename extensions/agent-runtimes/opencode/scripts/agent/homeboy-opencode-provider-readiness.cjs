#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const { openCodeRuntimeReadiness } = require('../../lib/opencode-provider-readiness');

try {
	const request = JSON.parse(fs.readFileSync(0, 'utf8'));
	process.stdout.write(`${JSON.stringify(openCodeRuntimeReadiness(request))}\n`);
} catch {
	// Do not serialize parser or process errors: either can contain secret input.
	process.stderr.write('OpenCode provider readiness request could not be processed.\n');
	process.exitCode = 1;
}
