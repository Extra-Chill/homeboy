#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';

const manifestPath = path.resolve(process.argv[2] || path.join(process.cwd(), 'wordpress.json'));
const env = process.env;

const overrides = {
	wp_codebox_bin: firstEnv('HOMEBOY_WP_CODEBOX_CLI', 'WP_CODEBOX_CLI', 'WP_CODEBOX_BIN', 'HOMEBOY_WP_CODEBOX_BIN'),
	wp_codebox_core_module: firstEnv('WP_CODEBOX_CORE_MODULE', 'HOMEBOY_WP_CODEBOX_CORE_MODULE'),
};

if (!overrides.wp_codebox_bin && !overrides.wp_codebox_core_module) {
	process.exit(0);
}

if (overrides.wp_codebox_bin && !isExecutableOrNodeEntrypoint(overrides.wp_codebox_bin)) {
	fail(`Explicit WP Codebox CLI override is not executable: ${overrides.wp_codebox_bin}`);
}
if (overrides.wp_codebox_core_module && !isFile(overrides.wp_codebox_core_module)) {
	fail(`Explicit WP Codebox core module override is not a file: ${overrides.wp_codebox_core_module}`);
}

const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
if (!Array.isArray(manifest.settings)) {
	fail(`Manifest does not declare settings: ${manifestPath}`);
}

let changed = false;
for (const [id, value] of Object.entries(overrides)) {
	if (!value) {
		continue;
	}
	const setting = manifest.settings.find((entry) => entry && entry.id === id);
	if (!setting) {
		fail(`Manifest is missing setting ${id}: ${manifestPath}`);
	}
	if (setting.default !== value) {
		setting.default = value;
		changed = true;
	}
}

if (changed) {
	fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
}

function firstEnv(...names) {
	for (const name of names) {
		const value = env[name];
		if (typeof value === 'string' && value.trim() !== '') {
			return value.trim();
		}
	}
	return '';
}

function isExecutableOrNodeEntrypoint(filePath) {
	if (!isFile(filePath)) {
		return false;
	}
	if (/\.(?:js|cjs|mjs)$/.test(filePath)) {
		return true;
	}
	try {
		fs.accessSync(filePath, fs.constants.X_OK);
		return true;
	} catch {
		return false;
	}
}

function isFile(filePath) {
	try {
		return fs.statSync(filePath).isFile();
	} catch {
		return false;
	}
}

function fail(message) {
	console.error(message);
	process.exit(1);
}
