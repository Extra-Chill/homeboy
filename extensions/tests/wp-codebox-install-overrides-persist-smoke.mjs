#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-install-overrides-'));
const manifestPath = path.join(tempDir, 'wordpress.json');
const cliPath = path.join(tempDir, 'wp-codebox-main-current', 'packages', 'cli', 'dist', 'index.js');
const coreModulePath = path.join(tempDir, 'wp-codebox-main-current', 'packages', 'runtime-core', 'dist', 'index.js');

fs.cpSync(path.join(rootDir, 'wordpress', 'wordpress.json'), manifestPath);
fs.mkdirSync(path.dirname(cliPath), { recursive: true });
fs.mkdirSync(path.dirname(coreModulePath), { recursive: true });
fs.writeFileSync(cliPath, '#!/usr/bin/env node\n');
fs.chmodSync(cliPath, 0o755);
fs.writeFileSync(coreModulePath, 'export {};\n');

const result = spawnSync(process.execPath, [
	path.join(rootDir, 'wordpress', 'scripts', 'build', 'persist-wp-codebox-overrides.mjs'),
	manifestPath,
], {
	env: {
		...process.env,
		WP_CODEBOX_CLI: cliPath,
		WP_CODEBOX_CORE_MODULE: coreModulePath,
	},
	encoding: 'utf8',
});
assert.equal(result.status, 0, result.stderr || result.stdout);

const installedManifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
const installedSettings = Object.fromEntries(installedManifest.settings.map((setting) => [setting.id, setting.default]));
assert.equal(installedSettings.wp_codebox_bin, cliPath);
assert.equal(installedSettings.wp_codebox_core_module, coreModulePath);

// Machine mode must write the same override values to a flat, machine-scoped
// file without touching the tracked manifest, so setup in a linked extension
// source checkout cannot dirty wordpress.json with machine-local paths.
const manifestBefore = fs.readFileSync(manifestPath, 'utf8');
const machineOverridesPath = path.join(tempDir, 'machine-overrides.json');
const machineResult = spawnSync(process.execPath, [
	path.join(rootDir, 'wordpress', 'scripts', 'build', 'persist-wp-codebox-overrides.mjs'),
	'--machine',
	machineOverridesPath,
	manifestPath,
], {
	env: {
		...process.env,
		WP_CODEBOX_CLI: cliPath,
		WP_CODEBOX_CORE_MODULE: coreModulePath,
	},
	encoding: 'utf8',
});
assert.equal(machineResult.status, 0, machineResult.stderr || machineResult.stdout);

const machineOverrides = JSON.parse(fs.readFileSync(machineOverridesPath, 'utf8'));
assert.equal(machineOverrides.wp_codebox_bin, cliPath);
assert.equal(machineOverrides.wp_codebox_core_module, coreModulePath);
assert.equal(fs.readFileSync(manifestPath, 'utf8'), manifestBefore, 'machine mode must not rewrite the manifest');

const { resolveRuntimeProvider } = require('../runtime-agent-ci/lib/runtime-provider-resolver.cjs');
const runtime = resolveRuntimeProvider('wp-codebox', {
	repoRoot: rootDir,
	workspace: tempDir,
	env: {
		HOMEBOY_SETTINGS_JSON: JSON.stringify({
			wp_codebox_bin: installedSettings.wp_codebox_bin,
			wp_codebox_core_module: installedSettings.wp_codebox_core_module,
		}),
	},
});
assert.equal(runtime.paths.runtime_bin, cliPath);
assert.equal(runtime.paths.runtime_core_module, coreModulePath);

console.log('wp-codebox install overrides persist smoke passed');
