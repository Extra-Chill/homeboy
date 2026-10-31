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
	'wp-codebox-resolver.js',
	'wp-codebox-runtime-contract-source.js',
	'wp-codebox-runtime-readiness.js',
]);
const legacyInternalPatterns = [
	/@automattic\/wp-codebox-core/,
	/wp-codebox-workspace/,
	/packages\/runtime-(?:core|playground)\/dist/,
	/packages\/cli\/dist/,
	/node_modules\/@automattic\/wp-codebox-core/,
	/runLegacyArtifactApplyPreflight/,
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
		"if (process.argv.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }",
		"if (process.argv.slice(-3).join(' ') === 'runtime descriptor --json') { process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } })); process.exit(0); }",
		`fs.writeFileSync(${JSON.stringify(capture)}, JSON.stringify({ argv: process.argv.slice(2) }, null, 2));`,
		'process.stdout.write(JSON.stringify({ ok: true }));',
		'',
	].join('\n'));
	fs.chmodSync(bin, 0o755);

	const client = createCodeboxClient({ wp_codebox_bin: bin, env: {} });
	assert.equal(client.identity().bin, bin);
	assert.deepEqual(client.publicCliInvocation(), { command: process.execPath, args: [bin] });
	assert.deepEqual(publicJsonArgs('run-fuzz-suite', '/tmp/input.json', { runnerMode: 'runtime-backed' }), ['run-fuzz-suite', '--runner-mode=runtime-backed', '--input-file', '/tmp/input.json', '--json']);
	assert.deepEqual(publicJsonArgs('run-fuzz-suite', '/tmp/input.json', { runnerMode: 'runtime-backed', artifactRoot: '/tmp/artifacts' }), ['run-fuzz-suite', '--runner-mode=runtime-backed', '--input-file', '/tmp/input.json', '--json', '--artifacts', '/tmp/artifacts']);
	assert.deepEqual(publicJsonArgs('run-fuzz-suite', '/tmp/input.json', { env: { artifactRoot: '/tmp/normalized-artifacts' } }), ['run-fuzz-suite', '--input-file', '/tmp/input.json', '--format=json', '--artifacts', '/tmp/normalized-artifacts']);
	assert.deepEqual(publicArtifactApplyPreflightArgs({
		bundlePath: '/tmp/bundle',
		approvedFiles: ['wp-content/plugins/example/readme.txt'],
	}), ['artifacts', 'apply-preflight', '--bundle', '/tmp/bundle', '--json', '--approved-file', 'wp-content/plugins/example/readme.txt']);

	const cliResult = client.runPublicCliCommand(['run-fuzz-suite', '--help']);
	assert.equal(cliResult.status, 0);
	assert.deepEqual(JSON.parse(fs.readFileSync(capture, 'utf8')).argv, ['run-fuzz-suite', '--help']);

	const missingVersion = path.join(root, 'missing-version.cjs');
	fs.writeFileSync(missingVersion, 'process.stdout.write("not-a-version");\n');
	fs.chmodSync(missingVersion, 0o755);
	assert.throws(
		() => createCodeboxClient({ wp_codebox_bin: missingVersion, env: {} }).runPublicCliCommand(['run-fuzz-suite']),
		/wp_codebox_version_probe_failed/
	);

	const missingDescriptor = path.join(root, 'missing-descriptor.cjs');
	fs.writeFileSync(missingDescriptor, 'if (process.argv.includes("--version")) process.stdout.write("0.21.0");\n');
	fs.chmodSync(missingDescriptor, 0o755);
	assert.throws(
		() => createCodeboxClient({ wp_codebox_bin: missingDescriptor, env: {} }).runPublicCliCommand(['run-fuzz-suite']),
		/wp_codebox_runtime_descriptor_invalid/
	);

	const managedInstall = path.join(root, 'managed');
	const managedBin = path.join(managedInstall, 'source', 'packages', 'cli', 'dist', 'index.js');
	fs.mkdirSync(path.dirname(managedBin), { recursive: true });
	fs.writeFileSync(managedBin, fs.readFileSync(bin));
	fs.mkdirSync(path.join(managedInstall, 'source', '.git'), { recursive: true });
	fs.writeFileSync(path.join(managedInstall, 'source', '.homeboy-runtime-identity.json'), JSON.stringify({
		schema: 'homeboy/wp-codebox-managed-runtime-identity/v1',
		source_sha: '0000000000000000000000000000000000000000',
		cli_sha256: '0'.repeat(64),
		required_capabilities: ['wp-codebox/browser-contained-site-open/v1'],
	}));
	assert.throws(
		() => createCodeboxClient({ env: { HOMEBOY_WP_CODEBOX_INSTALL_DIR: managedInstall } }).runPublicCliCommand(['run-fuzz-suite']),
		/wp_codebox_managed_source_identity_invalid/
	);

	const stalePath = path.join(root, 'stale-path');
	const staleBin = path.join(stalePath, 'wp-codebox');
	const staleMarker = path.join(root, 'stale-path-ran');
	fs.mkdirSync(stalePath);
	fs.writeFileSync(staleBin, `#!/usr/bin/env node
require('node:fs').writeFileSync(${JSON.stringify(staleMarker)}, 'ran');
process.stdout.write('0.12.0');
`);
	fs.chmodSync(staleBin, 0o755);
	const incompleteInstall = path.join(root, 'incomplete');
	fs.mkdirSync(path.join(incompleteInstall, 'source'), { recursive: true });
	assert.throws(
		() => createCodeboxClient({ env: { HOMEBOY_WP_CODEBOX_INSTALL_DIR: incompleteInstall, PATH: `${stalePath}:${process.env.PATH}` } }).runPublicCliCommand(['run-fuzz-suite']),
		/wp_codebox_managed_binary_missing/
	);
	assert.equal(fs.existsSync(staleMarker), false, 'an incomplete managed cache must not fall through to stale PATH');
	assert.equal(
		createCodeboxClient({ wp_codebox_bin: bin, env: { HOMEBOY_WP_CODEBOX_INSTALL_DIR: incompleteInstall, PATH: `${stalePath}:${process.env.PATH}` } }).runPublicCliCommand(['run-fuzz-suite']).status,
		0,
		'a configured pin wins over an incomplete managed cache'
	);
	const updatingInstall = path.join(root, 'updating');
	fs.mkdirSync(path.join(updatingInstall, 'source.update-lock'), { recursive: true });
	assert.throws(
		() => createCodeboxClient({ env: { HOMEBOY_WP_CODEBOX_INSTALL_DIR: updatingInstall, PATH: `${stalePath}:${process.env.PATH}` } }).runPublicCliCommand(['run-fuzz-suite']),
		/wp_codebox_managed_updating/
	);
	assert.equal(fs.existsSync(staleMarker), false, 'an update lock must not fall through to stale PATH');

	const delegatedResult = createCodeboxClient({
		wp_codebox_bin: bin,
		runPublicCli: ({ command, args }) => ({ status: 0, stdout: JSON.stringify({ command, args }) }),
	}).runPublicCliCommand(['run-wordpress-workload', '--help']);
	assert.equal(JSON.parse(delegatedResult.stdout).command, bin);

	const preflightResult = createCodeboxClient({
		wp_codebox_bin: bin,
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
		wp_codebox_bin: bin,
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
