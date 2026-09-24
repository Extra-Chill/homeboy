#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const { openCodeProviderReadiness } = require('../../lib/opencode-provider-readiness');

(async () => {
	const request = JSON.parse(fs.readFileSync(0, 'utf8'));
	process.stdout.write(`${JSON.stringify(await openCodeProviderReadiness(request))}\n`);
})().catch(() => {
	// Do not serialize parser or process errors: either can contain secret input.
	process.stderr.write('OpenCode provider readiness request could not be processed.\n');
	process.exitCode = 1;
});
