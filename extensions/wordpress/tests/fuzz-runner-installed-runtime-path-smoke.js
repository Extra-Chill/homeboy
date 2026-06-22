'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	resolveWpCodeboxRuntimePath,
	wpCodeboxRuntimeEnv,
} = require('../scripts/fuzz/fuzz-runner.cjs');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-fuzz-runner-runtime-path-'));
const homeboyRoot = path.join(root, '.config', 'homeboy');
const extensionPath = path.join(homeboyRoot, 'extensions', 'wordpress');
const installedRuntimePath = path.join(homeboyRoot, 'agent-runtimes', 'wp-codebox');
const legacyRuntimePath = path.join(homeboyRoot, 'extensions', 'agent-runtimes', 'wp-codebox');

fs.mkdirSync(extensionPath, { recursive: true });
fs.mkdirSync(installedRuntimePath, { recursive: true });
fs.mkdirSync(legacyRuntimePath, { recursive: true });
fs.writeFileSync(path.join(installedRuntimePath, 'index.js'), 'module.exports = {};\n');
fs.writeFileSync(path.join(legacyRuntimePath, 'index.js'), 'module.exports = {};\n');

assert.equal(
	resolveWpCodeboxRuntimePath({ env: { HOMEBOY_EXTENSION_PATH: extensionPath } }),
	installedRuntimePath
);

const sourceRuntimePath = path.resolve(__dirname, '..', '..', 'agent-runtimes', 'wp-codebox');
assert.equal(resolveWpCodeboxRuntimePath({ env: {} }), sourceRuntimePath);

assert.equal(
	wpCodeboxRuntimeEnv({
		HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_core_module: '/tmp/wp-codebox-core.mjs' }),
	}).HOMEBOY_WP_CODEBOX_CORE_MODULE,
	'/tmp/wp-codebox-core.mjs'
);
assert.equal(
	wpCodeboxRuntimeEnv({
		HOMEBOY_WP_CODEBOX_CORE_MODULE: '/existing/core.mjs',
		HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_core_module: '/tmp/wp-codebox-core.mjs' }),
	}).HOMEBOY_WP_CODEBOX_CORE_MODULE,
	'/existing/core.mjs'
);

console.log('fuzz runner installed runtime path smoke passed');
