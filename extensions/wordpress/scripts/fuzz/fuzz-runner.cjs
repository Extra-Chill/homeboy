#!/usr/bin/env node
'use strict';

/**
 * Internal dependencies
 */
const {
	buildWordPressFuzzRunnerResult,
	readWordPressFuzzRunnerEnv,
	writeHomeboyFuzzResultsFile,
} = require('../../lib/wordpress-fuzz-runner');

try {
	const env = readWordPressFuzzRunnerEnv();
	const result = buildWordPressFuzzRunnerResult({ env });
	writeHomeboyFuzzResultsFile(env.resultsFile, result.homeboy_fuzz_campaign);
	process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
} catch (error) {
	process.stderr.write(`${error.message}\n`);
	process.exit(1);
}
