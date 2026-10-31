'use strict';

const fs = require('node:fs');
const path = require('node:path');

// WP Codebox helpers travel with the extension in both checkouts and installs.
function resolveAgentRuntimeFile(relativePath) {
	const resolved = path.resolve(__dirname, '..', '..', 'wp-codebox-runtime', relativePath);
	if (!fs.existsSync(resolved)) {
		throw new Error(`Missing WordPress runtime helper: ${resolved}`);
	}
	return resolved;
}

function requireAgentRuntimeModule(relativePath) {
	return require(resolveAgentRuntimeFile(relativePath));
}

module.exports = { resolveAgentRuntimeFile, requireAgentRuntimeModule };

if (require.main === module) {
	if (!process.argv[2]) {
		process.stderr.write('Usage: agent-runtime-paths.cjs <runtime-file>\n');
		process.exit(2);
	}
	process.stdout.write(`${resolveAgentRuntimeFile(process.argv[2])}\n`);
}
