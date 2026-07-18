'use strict';

const fs = require('node:fs');
const path = require('node:path');

const DEFAULT_WP_CODEBOX_BIN = 'wp-codebox';

function homeboySettings(env = process.env) {
	try {
		const settings = JSON.parse(env.HOMEBOY_SETTINGS_JSON || '{}');
		return settings && typeof settings === 'object' && !Array.isArray(settings) ? settings : {};
	} catch {
		return {};
	}
}

function installedExtensionSettingDefaults(options = {}, env = process.env) {
	const manifestPath = options.extension_manifest_path || env.HOMEBOY_EXTENSION_MANIFEST_PATH || path.resolve(__dirname, '..', 'wordpress.json');
	try {
		const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
		return Object.fromEntries((manifest.settings || []).filter((setting) => setting?.id && setting.default !== undefined && setting.default !== '').map((setting) => [setting.id, setting.default]));
	} catch {
		return {};
	}
}

function wpCodeboxCommand(bin) {
	if (!bin || typeof bin !== 'string') throw new Error('WP Codebox binary is required. Set wp_codebox_bin or HOMEBOY_WP_CODEBOX_BIN.');
	return /\.(?:js|cjs|mjs)$/.test(bin) ? { command: process.execPath, args: [bin] } : { command: bin, args: [] };
}

function resolveWpCodeboxIdentity(options = {}) {
	const env = { ...process.env, ...(options.env || {}) };
	const settings = homeboySettings(env);
	const defaults = installedExtensionSettingDefaults(options, env);
	const bin = options.wp_codebox_bin || env.HOMEBOY_WP_CODEBOX_BIN || settings.wp_codebox_bin || defaults.wp_codebox_bin;
	if (!bin) throw new Error('WP Codebox binary is not configured. Set wp_codebox_bin in the installed WordPress extension manifest or HOMEBOY_WP_CODEBOX_BIN.');
	return { bin, invocation: wpCodeboxCommand(bin), selectionSource: options.wp_codebox_bin ? 'explicit' : env.HOMEBOY_WP_CODEBOX_BIN ? 'env' : settings.wp_codebox_bin ? 'settings' : 'manifest-default' };
}

module.exports = { DEFAULT_WP_CODEBOX_BIN, homeboySettings, installedExtensionSettingDefaults, resolveWpCodeboxIdentity, wpCodeboxCommand };
