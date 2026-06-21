#!/usr/bin/env node
'use strict';

/**
 * Internal dependencies
 */
const { buildWordPressFuzzRunnerResult, readWordPressFuzzRunnerEnv } = require('../../lib/wordpress-fuzz-runner');

try {
	const result = buildWordPressFuzzRunnerResult({ env: readWordPressFuzzRunnerEnv() });
	process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
} catch (error) {
	process.stderr.write(`${error.message}\n`);
	process.exit(1);
}
