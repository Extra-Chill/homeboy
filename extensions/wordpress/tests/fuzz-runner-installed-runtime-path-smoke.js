'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	resolveWpCodeboxRuntimePath,
	wpCodeboxRuntimeCommand,
	wpCodeboxRuntimeEnv,
} = require('../scripts/fuzz/fuzz-runner.cjs');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-fuzz-runner-runtime-path-'));
process.on('exit', () => fs.rmSync(root, { recursive: true, force: true }));
const homeboyRoot = path.join(root, '.config', 'homeboy');
const extensionPath = path.join(homeboyRoot, 'extensions', 'wordpress');
const installedRuntimePath = path.join(homeboyRoot, 'agent-runtimes', 'wp-codebox');
const legacyRuntimePath = path.join(homeboyRoot, 'extensions', 'agent-runtimes', 'wp-codebox');
const cachedCoreModule = path.join(root, 'wp-codebox-cache', 'source', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'contracts.js');
const cachedCoreIndex = path.join(root, 'wp-codebox-cache', 'source', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'index.js');
const manifestDefaultBin = path.join(root, 'wp-codebox-main-current', 'packages', 'cli', 'dist', 'index.js');
const manifestDefaultCoreModule = path.join(root, 'wp-codebox-main-current', 'packages', 'runtime-core', 'dist', 'index.js');
const isolatedRuntimeEnv = {
	HOME: path.join(root, 'home'),
	HOMEBOY_WP_CODEBOX_INSTALL_DIR: path.join(root, 'empty-wp-codebox-cache'),
};

fs.mkdirSync(extensionPath, { recursive: true });
fs.mkdirSync(installedRuntimePath, { recursive: true });
fs.mkdirSync(legacyRuntimePath, { recursive: true });
fs.writeFileSync(path.join(installedRuntimePath, 'index.js'), 'module.exports = {};\n');
fs.writeFileSync(path.join(legacyRuntimePath, 'index.js'), 'module.exports = {};\n');
fs.writeFileSync(path.join(extensionPath, 'wordpress.json'), JSON.stringify({
	settings: {
		wp_codebox_bin: { default: manifestDefaultBin },
		wp_codebox_core_module: { default: manifestDefaultCoreModule },
	},
}));
fs.mkdirSync(path.dirname(cachedCoreModule), { recursive: true });
fs.writeFileSync(cachedCoreModule, 'export {};\n');
fs.writeFileSync(cachedCoreIndex, 'export {};\n');

assert.equal(
	resolveWpCodeboxRuntimePath({ env: { HOMEBOY_EXTENSION_PATH: extensionPath } }),
	installedRuntimePath
);

const sourceRuntimePath = path.resolve(__dirname, '..', '..', 'agent-runtimes', 'wp-codebox');
assert.equal(resolveWpCodeboxRuntimePath({ env: {} }), sourceRuntimePath);

assert.equal(
	wpCodeboxRuntimeEnv({
		...isolatedRuntimeEnv,
		HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_core_module: '/tmp/wp-codebox-core.mjs' }),
	}).HOMEBOY_WP_CODEBOX_CORE_MODULE,
	'/tmp/wp-codebox-core.mjs'
);
assert.equal(
	wpCodeboxRuntimeEnv({
		...isolatedRuntimeEnv,
		HOMEBOY_EXTENSION_PATH: extensionPath,
	}).HOMEBOY_WP_CODEBOX_CORE_MODULE,
	manifestDefaultCoreModule
);
assert.equal(
	wpCodeboxRuntimeEnv({
		...isolatedRuntimeEnv,
		HOMEBOY_WP_CODEBOX_CORE_MODULE: '/existing/core.mjs',
		HOMEBOY_EXTENSION_PATH: extensionPath,
		HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_core_module: '/tmp/wp-codebox-core.mjs' }),
	}).HOMEBOY_WP_CODEBOX_CORE_MODULE,
	'/existing/core.mjs'
);
assert.equal(typeof wpCodeboxRuntimeCommand, 'function');
assert.equal(
	wpCodeboxRuntimeEnv({
		HOMEBOY_WP_CODEBOX_INSTALL_DIR: path.join(root, 'wp-codebox-cache'),
	}).HOMEBOY_WP_CODEBOX_CORE_MODULE,
	cachedCoreModule
);

console.log('fuzz runner installed runtime path smoke passed');
