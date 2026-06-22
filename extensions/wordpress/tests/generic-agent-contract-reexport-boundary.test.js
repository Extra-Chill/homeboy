'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const reexportOnlyModules = new Map([
	[
		'agent-task-runner-contract.js',
		"'use strict';\n\nmodule.exports = require('../../runtime-agent-ci/lib/agent-task-runner-contract');\n",
	],
	[
		'generic-agent-task-plan.js',
		"'use strict';\n\nmodule.exports = require('../../runtime-agent-ci/lib/generic-agent-task-plan');\n",
	],
]);

for (const [fileName, expectedSource] of reexportOnlyModules) {
	const source = fs.readFileSync(path.join(__dirname, '..', 'lib', fileName), 'utf8');
	assert.equal(source, expectedSource, `${fileName} must stay a runtime-agent-ci re-export.`);
}

process.stdout.write('Generic agent contract re-export boundary passed\n');
