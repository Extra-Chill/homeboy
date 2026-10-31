'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const repoRoot = path.join(__dirname, '..', '..');

const read = (relativePath) => fs.readFileSync(path.join(repoRoot, relativePath), 'utf8');

const readIfExists = (relativePath) => {
	const filePath = path.join(repoRoot, relativePath);
	return fs.existsSync(filePath) ? fs.readFileSync(filePath, 'utf8') : '';
};

const readDirectoryFiles = (directory, predicate) => {
	const root = path.join(repoRoot, directory);
	if (!fs.existsSync(root)) {
		return [];
	}
	return fs.readdirSync(root)
		.filter((entry) => predicate(entry))
		.map((entry) => read(path.join(directory, entry)));
};

const readme = read('README.md');
const wordpressDocs = read('docs/extensions/wordpress.md');

assert(readme.includes('core `homeboy fuzz plan`'), 'README must route workflow fuzz request composition through core homeboy fuzz plan.');
assert(wordpressDocs.includes('homeboy fuzz plan my-wordpress-component'), 'WordPress extension docs must include a core fuzz plan workflow example.');
assert(wordpressDocs.includes('wp-codebox/fuzz-suite/v1'), 'WordPress extension docs must preserve the WP Codebox fuzz-suite runtime boundary.');

const workflowAndScriptText = [
	...readDirectoryFiles('.github/workflows', (entry) => entry.endsWith('.yml') || entry.endsWith('.yaml')),
	...readDirectoryFiles('.github/scripts/runtime-agent-full-run', (entry) => entry.endsWith('.cjs') || entry.endsWith('.mjs') || entry.endsWith('.js')),
	readIfExists('wordpress/package.json'),
].join('\n');

assert(!/homeboy\s+fuzz\s+run/.test(workflowAndScriptText), 'HBX workflows/scripts should not compose homeboy fuzz run commands; use core homeboy fuzz plan upstream.');

const help = spawnSync('homeboy', ['fuzz', 'plan', '--help'], { encoding: 'utf8' });
if (help.error && help.error.code === 'ENOENT') {
	console.log('core fuzz plan boundary smoke skipped homeboy CLI probe because homeboy is not installed');
} else {
	assert.equal(help.status, 0, help.stderr || help.stdout);
	assert((help.stdout || help.stderr).includes('Build a fuzz execution request without executing it'));
}

console.log('core fuzz plan boundary smoke passed');
