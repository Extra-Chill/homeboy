'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	resolveWpCodeboxIdentity,
	wpCodeboxCommand,
} = require('../lib/wp-codebox-resolver');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-resolver-'));
const home = path.join(root, 'home');
const emptyPath = path.join(root, 'bin');
const manifestPath = path.join(root, 'wordpress.json');
const emptyManifestPath = path.join(root, 'empty-wordpress.json');
const envBin = path.join(root, 'env-wp-codebox.cjs');
const settingsBin = path.join(root, 'settings-wp-codebox');
const manifestBin = path.join(root, 'manifest-wp-codebox.mjs');

try {
	fs.mkdirSync(home, { recursive: true });
	fs.mkdirSync(emptyPath, { recursive: true });
	fs.writeFileSync(envBin, '#!/usr/bin/env node\n');
	fs.writeFileSync(settingsBin, '#!/bin/sh\n');
	fs.writeFileSync(manifestBin, '#!/usr/bin/env node\n');
	fs.writeFileSync(manifestPath, JSON.stringify({
		settings: [{ id: 'wp_codebox_bin', default: manifestBin }],
	}));
	fs.writeFileSync(emptyManifestPath, JSON.stringify({ settings: [] }));

	const isolatedEnv = {
		HOME: home,
		PATH: emptyPath,
		HOMEBOY_EXTENSION_MANIFEST_PATH: '',
		HOMEBOY_SETTINGS_JSON: '',
		HOMEBOY_WP_CODEBOX_BIN: '',
	};

	const envIdentity = resolveWpCodeboxIdentity({
		extension_manifest_path: manifestPath,
		env: {
			...isolatedEnv,
			HOMEBOY_WP_CODEBOX_BIN: envBin,
			HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: settingsBin }),
		},
	});
	assert.deepEqual(envIdentity, {
		bin: envBin,
		invocation: { command: process.execPath, args: [envBin] },
		selectionSource: 'env',
	});

	const explicitIdentity = resolveWpCodeboxIdentity({
		wp_codebox_bin: settingsBin,
		extension_manifest_path: manifestPath,
		env: { ...isolatedEnv, HOMEBOY_WP_CODEBOX_BIN: envBin },
	});
	assert.deepEqual(explicitIdentity, {
		bin: settingsBin,
		invocation: { command: settingsBin, args: [] },
		selectionSource: 'explicit',
	});

	const settingsIdentity = resolveWpCodeboxIdentity({
		extension_manifest_path: manifestPath,
		env: {
			...isolatedEnv,
			HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_bin: settingsBin }),
		},
	});
	assert.deepEqual(settingsIdentity, {
		bin: settingsBin,
		invocation: { command: settingsBin, args: [] },
		selectionSource: 'settings',
	});

	const manifestIdentity = resolveWpCodeboxIdentity({
		extension_manifest_path: manifestPath,
		env: isolatedEnv,
	});
	assert.deepEqual(manifestIdentity, {
		bin: manifestBin,
		invocation: { command: process.execPath, args: [manifestBin] },
		selectionSource: 'manifest-default',
	});

	assert.throws(
		() => resolveWpCodeboxIdentity({ extension_manifest_path: emptyManifestPath, env: isolatedEnv }),
		/WP Codebox binary is not configured/
	);
	assert.deepEqual(wpCodeboxCommand('wp-codebox'), { command: 'wp-codebox', args: [] });
	assert.deepEqual(wpCodeboxCommand(envBin), { command: process.execPath, args: [envBin] });

	console.log('wp-codebox resolver smoke passed');
} finally {
	fs.rmSync(root, { recursive: true, force: true });
}
