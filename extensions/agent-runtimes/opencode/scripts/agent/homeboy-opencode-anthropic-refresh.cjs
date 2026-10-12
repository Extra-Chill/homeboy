#!/usr/bin/env node
'use strict';

// Homeboy `controller_refresh` for the `anthropic-oauth` account
// (homeboy#15783). Exit 0 when the controller's Anthropic login is current;
// otherwise exit 1 with an operator-facing message on stderr. Never prints a
// token.

const { RefreshError, ensureFreshAnthropicLogin } = require('../../lib/opencode-anthropic-refresh');

ensureFreshAnthropicLogin().catch((error) => {
	process.stderr.write(
		error instanceof RefreshError
			? `${error.message}\n`
			: 'Anthropic OAuth refresh failed unexpectedly.\n',
	);
	process.exitCode = 1;
});
