#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';

// Usage: persist-wp-codebox-overrides.mjs [--machine <file>] <manifest>
//
// Default (manifest) mode rewrites the `default` of the wp_codebox_bin /
// wp_codebox_core_module settings in a wordpress.json-style manifest.
//
// `--machine <file>` instead writes the resolved override values to a flat,
// machine-scoped JSON file ({ wp_codebox_bin?, wp_codebox_core_module? })
// WITHOUT touching the tracked manifest. Setup uses this mode so that running
// it in a linked, git-managed extension source checkout never dirties
// wordpress.json with machine-local absolute paths.

const argv = process.argv.slice(2);
const machineIndex = argv.indexOf('--machine');
let machineFile = '';
if (machineIndex >= 0) {
	machineFile = path.resolve(argv[machineIndex + 1]);
	argv.splice(machineIndex, 2);
}
const manifestPath = path.resolve(argv[0] || path.join(process.cwd(), 'wordpress.json'));
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

for (const [id, value] of Object.entries(overrides)) {
	if (!value) {
		continue;
	}
	const setting = manifest.settings.find((entry) => entry && entry.id === id);
	if (!setting) {
		fail(`Manifest is missing setting ${id}: ${manifestPath}`);
	}
}

if (machineFile) {
	const machineOverrides = {};
	for (const [id, value] of Object.entries(overrides)) {
		if (value) {
			machineOverrides[id] = value;
		}
	}
	if (Object.keys(machineOverrides).length > 0) {
		fs.mkdirSync(path.dirname(machineFile), { recursive: true });
		const tmpFile = `${machineFile}.tmp`;
		fs.writeFileSync(tmpFile, `${JSON.stringify(machineOverrides, null, 2)}\n`);
		fs.renameSync(tmpFile, machineFile);
	}
	process.exit(0);
}

let changed = false;
for (const [id, value] of Object.entries(overrides)) {
	if (!value) {
		continue;
	}
	const setting = manifest.settings.find((entry) => entry && entry.id === id);
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
