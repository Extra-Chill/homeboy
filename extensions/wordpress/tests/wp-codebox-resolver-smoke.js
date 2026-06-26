'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	resolveWpCodeboxIdentity,
	wpCodeboxIdentityMismatchDiagnostics,
} = require('../lib/wp-codebox-resolver');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-resolver-'));

function makeCodeboxRoot(name, version = '1.2.3') {
	const sourceRoot = path.join(root, name);
	const cliDist = path.join(sourceRoot, 'packages', 'cli', 'dist');
	const coreDist = path.join(sourceRoot, 'packages', 'runtime-core', 'dist');
	const runtimeDist = path.join(sourceRoot, 'packages', 'runtime-playground', 'dist');
	fs.mkdirSync(cliDist, { recursive: true });
	fs.mkdirSync(coreDist, { recursive: true });
	fs.mkdirSync(runtimeDist, { recursive: true });
	fs.writeFileSync(path.join(sourceRoot, 'package.json'), JSON.stringify({ version }));
	fs.writeFileSync(path.join(cliDist, 'index.js'), '#!/usr/bin/env node\n');
	fs.writeFileSync(path.join(coreDist, 'index.js'), 'export const ok = true;\n');
	fs.writeFileSync(path.join(runtimeDist, 'index.js'), 'export const runtime = true;\n');
	return sourceRoot;
}

try {
	const envRoot = makeCodeboxRoot('wp-codebox@env', '1.0.0');
	const settingsRoot = makeCodeboxRoot('wp-codebox@settings', '2.0.0');
	const cacheRoot = path.join(root, 'cache', 'wp-codebox');
	const cacheSourceRoot = makeCodeboxRoot(path.join('cache', 'wp-codebox', 'source'), '3.0.0');
	const workspaceRoot = path.join(root, 'workspace');
	const workspaceSourceRoot = makeCodeboxRoot(path.join('workspace', 'wp-codebox@feature'), '4.0.0');
	fs.mkdirSync(cacheRoot, { recursive: true });
	fs.mkdirSync(workspaceRoot, { recursive: true });

	const envBin = path.join(envRoot, 'packages', 'cli', 'dist', 'index.js');
	const settingsBin = path.join(settingsRoot, 'packages', 'cli', 'dist', 'index.js');

	const envIdentity = resolveWpCodeboxIdentity({
		env: {
			HOMEBOY_WP_CODEBOX_BIN: envBin,
			HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: settingsBin }),
		},
	});
	assert.equal(envIdentity.selectionSource, 'env');
	assert.equal(envIdentity.bin, envBin);
	assert.equal(envIdentity.sourceRoot, envRoot);
	assert.equal(envIdentity.coreModulePath, path.join(envRoot, 'packages', 'runtime-core', 'dist', 'index.js'));
	assert.equal(envIdentity.runtimePackagePath, path.join(envRoot, 'packages', 'runtime-playground', 'dist', 'index.js'));
	assert.equal(envIdentity.fingerprint.version, '1.0.0');
	assert.deepEqual(envIdentity.invocation, { command: process.execPath, args: [envBin] });

	const settingsIdentity = resolveWpCodeboxIdentity({
		env: { HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: settingsBin }) },
	});
	assert.equal(settingsIdentity.selectionSource, 'settings');
	assert.equal(settingsIdentity.bin, settingsBin);
	assert.equal(settingsIdentity.sourceRoot, settingsRoot);

	const cacheIdentity = resolveWpCodeboxIdentity({
		wpCodeboxInstallDir: cacheRoot,
		env: {},
	});
	assert.equal(cacheIdentity.selectionSource, 'cache');
	assert.equal(cacheIdentity.installRoot, cacheRoot);
	assert.equal(cacheIdentity.sourceRoot, cacheSourceRoot);
	assert.equal(cacheIdentity.bin, path.join(cacheSourceRoot, 'packages', 'cli', 'dist', 'index.js'));

	const workspaceIdentity = resolveWpCodeboxIdentity({
		workspaceRoot,
		wpCodeboxInstallDir: path.join(root, 'missing-cache'),
		env: {},
	});
	assert.equal(workspaceIdentity.selectionSource, 'workspace');
	assert.equal(workspaceIdentity.sourceRoot, workspaceSourceRoot);
	assert.equal(workspaceIdentity.bin, path.join(workspaceSourceRoot, 'packages', 'cli', 'dist', 'index.js'));

	const mismatchIdentity = resolveWpCodeboxIdentity({
		env: { HOMEBOY_WP_CODEBOX_BIN: envBin },
		coreModule: path.join(settingsRoot, 'packages', 'runtime-core', 'dist', 'index.js'),
	});
	const diagnostics = wpCodeboxIdentityMismatchDiagnostics(mismatchIdentity);
	assert.equal(diagnostics.length, 1);
	assert.equal(diagnostics[0].code, 'wp_codebox_identity_mismatch');
	assert.match(diagnostics[0].message, /one checkout supplies all WP Codebox paths/);
	assert.deepEqual(diagnostics[0].locations.map((entry) => entry.role).sort(), ['cli', 'core', 'runtime']);

	console.log('wp-codebox resolver smoke passed');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
