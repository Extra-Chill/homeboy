'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	createCodeboxClient,
	publicArtifactApplyPreflightArgs,
	publicJsonArgs,
} = require('../lib/codebox-client');

const wordpressRoot = path.resolve(__dirname, '..');
const libRoot = path.join(wordpressRoot, 'lib');
const allowedInternalFiles = new Set([
	'codebox-client.js',
	'wp-codebox-core-loader.js',
	'wp-codebox-resolver.js',
]);
const legacyInternalPatterns = [
	/@automattic\/wp-codebox-core/,
	/wp-codebox-workspace/,
	/packages\/runtime-(?:core|playground)\/dist/,
	/packages\/cli\/dist/,
	/node_modules\/@automattic\/wp-codebox-core/,
	/loadWpCodeboxCore(?:Export|Function)?/,
];

function walkJavaScriptFiles(directory) {
	const entries = fs.readdirSync(directory, { withFileTypes: true });
	return entries.flatMap((entry) => {
		const filePath = path.join(directory, entry.name);
		if (entry.isDirectory()) {
			return walkJavaScriptFiles(filePath);
		}
		return entry.isFile() && entry.name.endsWith('.js') ? [filePath] : [];
	});
}

for (const filePath of walkJavaScriptFiles(libRoot)) {
	const relative = path.relative(libRoot, filePath);
	if (allowedInternalFiles.has(relative)) {
		continue;
	}
	const source = fs.readFileSync(filePath, 'utf8');
	for (const pattern of legacyInternalPatterns) {
		assert.doesNotMatch(source, pattern, `${relative} must use CodeboxClient instead of Codebox internals matching ${pattern}`);
	}
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-client-boundary-'));
try {
	const bin = path.join(root, 'wp-codebox.cjs');
	const capture = path.join(root, 'capture.json');
	fs.writeFileSync(bin, [
		'#!/usr/bin/env node',
		"const fs = require('node:fs');",
		`fs.writeFileSync(${JSON.stringify(capture)}, JSON.stringify({ argv: process.argv.slice(2) }, null, 2));`,
		'process.stdout.write(JSON.stringify({ ok: true }));',
		'',
	].join('\n'));
	fs.chmodSync(bin, 0o755);

	const client = createCodeboxClient({ wpCodeboxBin: bin, env: {} });
	assert.equal(client.identity().bin, bin);
	assert.deepEqual(client.publicCliInvocation(), { command: process.execPath, args: [bin] });
	assert.deepEqual(publicJsonArgs('run-fuzz-suite', '/tmp/input.json', { runnerMode: 'runtime-backed' }), ['run-fuzz-suite', '--runner-mode=runtime-backed', '--input-file', '/tmp/input.json', '--json']);
	assert.deepEqual(publicArtifactApplyPreflightArgs({
		bundlePath: '/tmp/bundle',
		approvedFiles: ['wp-content/plugins/example/readme.txt'],
	}), ['artifacts', 'apply-preflight', '--bundle', '/tmp/bundle', '--json', '--approved-file', 'wp-content/plugins/example/readme.txt']);

	const cliResult = client.runPublicCliCommand(['run-fuzz-suite', '--help']);
	assert.equal(cliResult.status, 0);
	assert.deepEqual(JSON.parse(fs.readFileSync(capture, 'utf8')).argv, ['run-fuzz-suite', '--help']);

	const delegatedResult = createCodeboxClient({
		wpCliBin: 'wp',
		runPublicCli: ({ command, args }) => ({ status: 0, stdout: JSON.stringify({ command, args }) }),
	}).runPublicCliCommand(['run-wordpress-workload', '--help']);
	assert.equal(JSON.parse(delegatedResult.stdout).command, 'wp');

	const preflightResult = createCodeboxClient({
		wpCodeboxBin: bin,
		runPublicCli: ({ command, args }) => ({
			status: 0,
			stdout: JSON.stringify({ ready: true, command, args }),
		}),
	}).runArtifactApplyPreflight({
		bundlePath: '/tmp/bundle',
		approvedFiles: ['wp-content/plugins/example/readme.txt'],
	});
	assert.equal(preflightResult.ready, true);
	assert.deepEqual(preflightResult.args, ['artifacts', 'apply-preflight', '--bundle', '/tmp/bundle', '--json', '--approved-file', 'wp-content/plugins/example/readme.txt']);

	const discoveryResult = createCodeboxClient({
		wpCodeboxBin: bin,
		runPublicCli: ({ args }) => ({ status: 0, stdout: JSON.stringify({ artifacts: [], args }) }),
	}).runArtifactsDiscoverPartial({
		artifactsRoot: '/tmp/artifacts',
		sessionId: 'session-1',
		startedAt: '2026-01-01T00:00:00.000Z',
		finishedAt: '2026-01-01T00:00:01.000Z',
	});
	assert.deepEqual(discoveryResult.args, ['artifacts', 'discover-partial', '--artifacts', '/tmp/artifacts', '--json', '--session-id', 'session-1', '--started-at', '2026-01-01T00:00:00.000Z', '--finished-at', '2026-01-01T00:00:01.000Z']);

	console.log('codebox client boundary passed');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
