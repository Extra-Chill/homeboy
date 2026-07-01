'use strict';

/**
 * External dependencies
 */
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const DEFAULT_WP_CODEBOX_BIN = 'wp-codebox';
const DEFAULT_WP_CLI_BIN = 'wp';
const DEFAULT_CORE_MODULE = '@automattic/wp-codebox-core';
const DEFAULT_RUNTIME_CORE_ENTRY = 'packages/runtime-core/dist/contracts.js';
const DEFAULT_RUNTIME_CORE_FALLBACK_ENTRY = 'packages/runtime-core/dist/index.js';
const DEFAULT_RUNTIME_PLAYGROUND_ENTRY = 'packages/runtime-playground/dist/index.js';
const DEFAULT_CLI_ENTRY = 'packages/cli/dist/index.js';

function homeboySettings(env = process.env) {
	if (!env.HOMEBOY_SETTINGS_JSON) {
		return {};
	}

	try {
		const parsed = JSON.parse(env.HOMEBOY_SETTINGS_JSON);
		return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
	} catch {
		return {};
	}
}

function wpCodeboxCommand(bin = DEFAULT_WP_CODEBOX_BIN) {
	if (/\.(?:js|cjs|mjs)$/.test(String(bin || ''))) {
		return { command: process.execPath, args: [bin] };
	}

	return { command: bin, args: [] };
}

function resolveWpCodeboxIdentity(options = {}) {
	if (options.wpCodeboxIdentity && typeof options.wpCodeboxIdentity === 'object') {
		return options.wpCodeboxIdentity;
	}

	const env = { ...process.env, ...(options.env || {}) };
	const settings = homeboySettings(env);
	const manifestDefaults = installedExtensionSettingDefaults(options, env);
	const selection = selectWpCodeboxSource(options, env, settings, manifestDefaults);
	const sourceRoot = resolveSourceRoot(selection, options, env, settings);
	const installRoot = resolveInstallRoot(selection, sourceRoot, options, env, settings);
	const coreModulePath = resolveCoreModulePath(sourceRoot, installRoot, options, env, manifestDefaults);
	const runtimePackagePath = resolveRuntimePackagePath(sourceRoot, installRoot, options, env);
	const bin = selection.bin || resolveBinFromRoots(sourceRoot, installRoot) || DEFAULT_WP_CODEBOX_BIN;
	const invocation = wpCodeboxInvocation(bin, options);
	const fingerprint = wpCodeboxFingerprint({ sourceRoot, installRoot, bin, coreModulePath, runtimePackagePath });
	const identity = stripUndefined({
		bin,
		invocation,
		installRoot,
		sourceRoot,
		coreModulePath,
		runtimePackagePath,
		fingerprint,
		selectionSource: selection.source,
		selection: stripUndefined({ source: selection.source, path: selection.path || bin }),
	});
	const diagnostics = wpCodeboxIdentityMismatchDiagnostics(identity);
	return diagnostics.length > 0 ? { ...identity, diagnostics } : identity;
}

function selectWpCodeboxSource(options, env, settings, manifestDefaults = {}) {
	const explicitBin = firstString(options.wpCodeboxBin, options.wp_codebox_bin, options.publicCliBin, options.public_cli_bin, options.bin);
	if (explicitBin) {
		return { source: 'explicit', bin: explicitBin, path: explicitBin };
	}

	const envBin = firstString(env.HOMEBOY_WP_CODEBOX_BIN, env.WP_CODEBOX_BIN, env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN);
	if (envBin) {
		const configuredCacheRoot = firstExistingDirectory(configuredCacheRootCandidates(options, env, settings));
		if (configuredCacheRoot && !pathIsInside(envBin, configuredCacheRoot)) {
			return { source: 'cache', path: configuredCacheRoot };
		}
		return { source: 'env', bin: envBin, path: envBin };
	}

	const settingsBin = firstString(settings.wp_codebox_bin, settings.wpCodeboxBin);
	if (settingsBin) {
		return { source: 'settings', bin: settingsBin, path: settingsBin };
	}

	const manifestDefaultBin = firstString(manifestDefaults.wp_codebox_bin, manifestDefaults.wpCodeboxBin);
	if (manifestDefaultBin) {
		return { source: 'manifest-default', bin: manifestDefaultBin, path: manifestDefaultBin };
	}

	const explicitSourceRoot = firstString(options.wpCodeboxSourceRoot, options.wp_codebox_source_root, options.sourceRoot, options.source_root);
	if (explicitSourceRoot) {
		return { source: 'explicit', path: explicitSourceRoot };
	}

	const envSourceRoot = firstString(env.HOMEBOY_WP_CODEBOX_SOURCE_ROOT, env.WP_CODEBOX_SOURCE_ROOT);
	if (envSourceRoot) {
		return { source: 'env', path: envSourceRoot };
	}

	const settingsSourceRoot = firstString(settings.wp_codebox_source_root, settings.wpCodeboxSourceRoot);
	if (settingsSourceRoot) {
		return { source: 'settings', path: settingsSourceRoot };
	}

	const manifestDefaultSourceRoot = firstString(manifestDefaults.wp_codebox_source_root, manifestDefaults.wpCodeboxSourceRoot);
	if (manifestDefaultSourceRoot) {
		return { source: 'manifest-default', path: manifestDefaultSourceRoot };
	}

	const cacheRoot = firstExistingDirectory(cacheRootCandidates(options, env, settings));
	if (cacheRoot) {
		return { source: 'cache', path: cacheRoot };
	}

	const workspaceRoot = firstExistingDirectory(workspaceRepoCandidates(options, env));
	if (workspaceRoot) {
		return { source: 'workspace', path: workspaceRoot };
	}

	return { source: 'default', bin: DEFAULT_WP_CODEBOX_BIN, path: DEFAULT_WP_CODEBOX_BIN };
}

function configuredCacheRootCandidates(options, env, settings) {
	const explicit = firstString(
		options.wpCodeboxInstallDir,
		options.wpCodeboxInstallRoot,
		options.installRoot,
		env.HOMEBOY_WP_CODEBOX_INSTALL_DIR,
		env.HOMEBOY_WP_CODEBOX_INSTALL_ROOT,
		settings.wp_codebox_install_dir,
		settings.wpCodeboxInstallDir
	);
	return explicit ? [explicit] : [];
}

function resolveSourceRoot(selection, options, env, settings) {
	const explicit = firstExistingDirectory([
		options.wpCodeboxSourceRoot,
		options.wp_codebox_source_root,
		options.sourceRoot,
		options.source_root,
		env.HOMEBOY_WP_CODEBOX_SOURCE_ROOT,
		env.WP_CODEBOX_SOURCE_ROOT,
		settings.wp_codebox_source_root,
		settings.wpCodeboxSourceRoot,
	]);
	if (explicit) {
		return explicit;
	}
	if (selection.source === 'cache') {
		const source = path.resolve(selection.path, 'source');
		return directoryExists(source) ? source : undefined;
	}
	return sourceRootFromPath(selection.path || selection.bin);
}

function resolveInstallRoot(selection, sourceRoot, options, env, settings) {
	const explicit = firstString(
		options.wpCodeboxInstallDir,
		options.wpCodeboxInstallRoot,
		options.installRoot,
		env.HOMEBOY_WP_CODEBOX_INSTALL_DIR,
		env.HOMEBOY_WP_CODEBOX_INSTALL_ROOT,
		settings.wp_codebox_install_dir,
		settings.wpCodeboxInstallDir
	);
	if (explicit) {
		return path.resolve(expandHome(explicit));
	}
	if (selection.source === 'cache') {
		return path.resolve(selection.path);
	}
	if (sourceRoot) {
		const parent = path.dirname(sourceRoot);
		if (path.basename(sourceRoot) === 'source' && path.basename(parent) === 'wp-codebox') {
			return parent;
		}
	}
	return undefined;
}

function resolveCoreModulePath(sourceRoot, installRoot, options, env, manifestDefaults = {}) {
	const explicit = firstString(options.wpCodeboxCoreModule, options.coreModule);
	if (explicit) {
		return isPathSpecifier(explicit) ? path.resolve(expandHome(explicit)) : explicit;
	}
	const discovered = firstExistingFile([
		sourceRoot && path.resolve(sourceRoot, options.runtimeCoreEntry || DEFAULT_RUNTIME_CORE_ENTRY),
		sourceRoot && path.resolve(sourceRoot, DEFAULT_RUNTIME_CORE_FALLBACK_ENTRY),
		installRoot && path.resolve(installRoot, 'source', options.runtimeCoreEntry || DEFAULT_RUNTIME_CORE_ENTRY),
		installRoot && path.resolve(installRoot, 'source', DEFAULT_RUNTIME_CORE_FALLBACK_ENTRY),
		installRoot && path.resolve(installRoot, 'release', 'wp-codebox-cli', options.runtimeCoreEntry || DEFAULT_RUNTIME_CORE_ENTRY),
		installRoot && path.resolve(installRoot, 'release', 'wp-codebox-cli', DEFAULT_RUNTIME_CORE_FALLBACK_ENTRY),
		installRoot && path.resolve(installRoot, 'source', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'contracts.js'),
		installRoot && path.resolve(installRoot, 'source', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'index.js'),
		installRoot && path.resolve(installRoot, 'release', 'wp-codebox-cli', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'contracts.js'),
		installRoot && path.resolve(installRoot, 'release', 'wp-codebox-cli', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'index.js'),
	]);
	const envExplicit = firstString(env.HOMEBOY_WP_CODEBOX_CORE_MODULE, env.WP_CODEBOX_CORE_MODULE);
	if (envExplicit) {
		const resolvedEnvExplicit = isPathSpecifier(envExplicit) ? path.resolve(expandHome(envExplicit)) : envExplicit;
		if (!discovered || !isPathSpecifier(resolvedEnvExplicit) || pathIsInside(resolvedEnvExplicit, sourceRoot) || pathIsInside(resolvedEnvExplicit, installRoot)) {
			return resolvedEnvExplicit;
		}
		return discovered;
	}
	const manifestDefault = firstString(manifestDefaults.wp_codebox_core_module, manifestDefaults.wpCodeboxCoreModule);
	if (manifestDefault) {
		return isPathSpecifier(manifestDefault) ? path.resolve(expandHome(manifestDefault)) : manifestDefault;
	}
	return discovered || DEFAULT_CORE_MODULE;
}

function installedExtensionSettingDefaults(options = {}, env = process.env) {
	const manifest = readInstalledExtensionManifest(options, env);
	const settings = manifest?.settings;
	const defaults = {};
	if (Array.isArray(settings)) {
		for (const setting of settings) {
			if (!setting || typeof setting !== 'object' || typeof setting.id !== 'string' || setting.default === undefined || setting.default === '') {
				continue;
			}
			defaults[setting.id] = setting.default;
		}
		return defaults;
	}
	if (settings && typeof settings === 'object') {
		for (const [id, setting] of Object.entries(settings)) {
			const value = setting && typeof setting === 'object' && Object.hasOwn(setting, 'default') ? setting.default : undefined;
			if (value !== undefined && value !== '') {
				defaults[id] = value;
			}
		}
	}
	return defaults;
}

function readInstalledExtensionManifest(options = {}, env = process.env) {
	const explicitPath = firstString(options.extensionManifestPath, options.wordpressManifestPath, env.HOMEBOY_EXTENSION_MANIFEST_PATH);
	const extensionPath = firstString(options.extensionPath, env.HOMEBOY_EXTENSION_PATH);
	const candidates = [
		explicitPath,
		extensionPath && path.resolve(expandHome(extensionPath), 'wordpress.json'),
		path.resolve(__dirname, '..', 'wordpress.json'),
	];
	for (const candidate of candidates.filter(Boolean)) {
		try {
			const parsed = JSON.parse(fs.readFileSync(path.resolve(expandHome(candidate)), 'utf8'));
			if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
				return parsed;
			}
		} catch {
			// Missing or invalid manifests do not contribute defaults.
		}
	}
	return {};
}

function resolveRuntimePackagePath(sourceRoot, installRoot, options, env) {
	const explicit = firstString(options.wpCodeboxRuntimePath, options.runtimePackagePath, options.runtime_package_path, env.HOMEBOY_WP_CODEBOX_RUNTIME_PATH, env.WP_CODEBOX_RUNTIME_PATH);
	if (explicit) {
		return path.resolve(expandHome(explicit));
	}
	return firstExistingFile([
		sourceRoot && path.resolve(sourceRoot, options.runtimePlaygroundEntry || DEFAULT_RUNTIME_PLAYGROUND_ENTRY),
		installRoot && path.resolve(installRoot, 'source', options.runtimePlaygroundEntry || DEFAULT_RUNTIME_PLAYGROUND_ENTRY),
		installRoot && path.resolve(installRoot, 'release', 'wp-codebox-cli', options.runtimePlaygroundEntry || DEFAULT_RUNTIME_PLAYGROUND_ENTRY),
	]);
}

function resolveBinFromRoots(sourceRoot, installRoot) {
	return firstExistingFile([
		sourceRoot && path.resolve(sourceRoot, DEFAULT_CLI_ENTRY),
		installRoot && path.resolve(installRoot, 'source', DEFAULT_CLI_ENTRY),
		installRoot && path.resolve(installRoot, 'release', 'wp-codebox-cli', DEFAULT_CLI_ENTRY),
	]);
}

function wpCodeboxInvocation(bin, options = {}) {
	const invocation = wpCodeboxCommand(bin);
	const executable = path.basename(String(bin || '')).toLowerCase();
	const usesWpCliNamespace = options.wpCliNamespace === true || executable === 'wp' || executable === 'wp-cli' || executable === 'wp-cli.phar';
	return {
		command: invocation.command,
		args: usesWpCliNamespace ? [...invocation.args, 'codebox'] : invocation.args,
	};
}

function wpCodeboxIdentityMismatchDiagnostics(identity = {}) {
	const locations = [
		{ role: 'cli', path: identity.bin },
		{ role: 'core', path: identity.coreModulePath },
		{ role: 'runtime', path: identity.runtimePackagePath },
	]
		.map((entry) => ({ ...entry, sourceRoot: sourceRootFromPath(entry.path) }))
		.filter((entry) => entry.sourceRoot);

	const roots = [...new Set(locations.map((entry) => entry.sourceRoot))];
	if (roots.length <= 1) {
		return [];
	}

	return [{
		severity: 'error',
		code: 'wp_codebox_identity_mismatch',
		message: 'WP Codebox CLI, core module, and runtime package resolve to different source roots. Set HOMEBOY_WP_CODEBOX_BIN, HOMEBOY_WP_CODEBOX_SOURCE_ROOT, or Homeboy wp_codebox_* settings so one checkout supplies all WP Codebox paths.',
		locations,
	}];
}

function assertWpCodeboxIdentityMatches(identity) {
	const diagnostics = wpCodeboxIdentityMismatchDiagnostics(identity);
	if (diagnostics.length === 0) {
		return identity;
	}
	const error = new Error(diagnostics[0].message);
	error.code = diagnostics[0].code;
	error.diagnostics = diagnostics;
	throw error;
}

function cacheRootCandidates(options, env, settings) {
	const explicit = firstString(
		options.wpCodeboxInstallDir,
		options.wpCodeboxInstallRoot,
		options.installRoot,
		env.HOMEBOY_WP_CODEBOX_INSTALL_DIR,
		env.HOMEBOY_WP_CODEBOX_INSTALL_ROOT,
		settings.wp_codebox_install_dir,
		settings.wpCodeboxInstallDir
	);
	if (explicit) {
		return [explicit];
	}
	return [
		options.wpCodeboxInstallDir,
		options.wpCodeboxInstallRoot,
		options.installRoot,
		env.HOMEBOY_WP_CODEBOX_INSTALL_DIR,
		env.HOMEBOY_WP_CODEBOX_INSTALL_ROOT,
		settings.wp_codebox_install_dir,
		settings.wpCodeboxInstallDir,
		path.resolve(os.homedir(), '.cache', 'homeboy', 'wp-codebox'),
	];
}

function workspaceRepoCandidates(options, env) {
	const explicitRoot = firstString(options.workspaceRoot, env.HOMEBOY_WORKSPACE_ROOT, env.HOMEBOY_DEVELOPER_WORKSPACE);
	const roots = explicitRoot ? [explicitRoot] : unique([
		path.resolve(__dirname, '..', '..', '..'),
	]);
	const candidates = [];
	for (const root of roots) {
		const absoluteRoot = path.resolve(expandHome(root));
		candidates.push(path.resolve(absoluteRoot, 'wp-codebox'));
		try {
			for (const entry of fs.readdirSync(absoluteRoot, { withFileTypes: true })) {
				if (entry.isDirectory() && entry.name.startsWith('wp-codebox@')) {
					candidates.push(path.resolve(absoluteRoot, entry.name));
				}
			}
		} catch {
			// Unreadable workspace roots contribute no fallback candidates.
		}
	}
	return candidates.sort();
}

function sourceRootFromPath(value) {
	if (typeof value !== 'string' || !value.trim() || !isPathSpecifier(value)) {
		return undefined;
	}
	const absolute = path.resolve(expandHome(value));
	const normalized = absolute.split(path.sep);
	const packagesIndex = normalized.lastIndexOf('packages');
	if (packagesIndex > 0) {
		return normalized.slice(0, packagesIndex).join(path.sep) || path.sep;
	}
	if (path.basename(absolute) === 'source' && path.basename(path.dirname(absolute)) === 'wp-codebox') {
		return absolute;
	}
	if (directoryExists(path.resolve(absolute, 'packages'))) {
		return absolute;
	}
	return undefined;
}

function pathIsInside(value, root) {
	if (typeof value !== 'string' || !value.trim() || typeof root !== 'string' || !root.trim()) {
		return false;
	}
	const resolvedValue = path.resolve(expandHome(value));
	const resolvedRoot = path.resolve(expandHome(root));
	return resolvedValue === resolvedRoot || resolvedValue.startsWith(`${resolvedRoot}${path.sep}`);
}

function wpCodeboxFingerprint({ sourceRoot, installRoot, bin, coreModulePath, runtimePackagePath } = {}) {
	const packageJson = firstExistingFile([
		sourceRoot && path.resolve(sourceRoot, 'package.json'),
		installRoot && path.resolve(installRoot, 'source', 'package.json'),
	]);
	const packageVersion = readPackageVersion(packageJson);
	const git = sourceRoot && directoryExists(path.resolve(sourceRoot, '.git')) ? gitFingerprint(sourceRoot) : undefined;
	return stripUndefined({
		version: packageVersion,
		git,
		build: buildFingerprint([bin, coreModulePath, runtimePackagePath]),
	});
}

function readPackageVersion(packageJson) {
	if (!packageJson) {
		return undefined;
	}
	try {
		const parsed = JSON.parse(fs.readFileSync(packageJson, 'utf8'));
		return typeof parsed.version === 'string' && parsed.version.trim() ? parsed.version.trim() : undefined;
	} catch {
		return undefined;
	}
}

function gitFingerprint(root) {
	try {
		return execFileSync('git', ['rev-parse', '--short=12', 'HEAD'], { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim() || undefined;
	} catch {
		return undefined;
	}
}

function buildFingerprint(paths) {
	const entries = paths.filter((entry) => typeof entry === 'string' && isPathSpecifier(entry));
	const stats = entries.map((entry) => {
		try {
			const stat = fs.statSync(path.resolve(expandHome(entry)));
			return `${entry}:${stat.size}:${Math.trunc(stat.mtimeMs)}`;
		} catch {
			return undefined;
		}
	}).filter(Boolean);
	return stats.length > 0 ? stats.join('|') : undefined;
}

function isPathSpecifier(specifier) {
	return specifier.startsWith('.') || specifier.startsWith('~') || path.isAbsolute(specifier) || specifier.includes(path.sep) || specifier.includes('\\');
}

function firstString(...values) {
	for (const value of values) {
		if (typeof value === 'string' && value.trim()) {
			return value.trim();
		}
	}
	return undefined;
}

function firstExistingDirectory(values) {
	for (const value of values.filter(Boolean)) {
		const absolute = path.resolve(expandHome(value));
		if (directoryExists(absolute)) {
			return absolute;
		}
	}
	return undefined;
}

function firstExistingFile(values) {
	for (const value of values.filter(Boolean)) {
		const absolute = path.resolve(expandHome(value));
		if (fileExists(absolute)) {
			return absolute;
		}
	}
	return undefined;
}

function fileExists(value) {
	try {
		return fs.statSync(value).isFile();
	} catch {
		return false;
	}
}

function directoryExists(value) {
	try {
		return fs.statSync(value).isDirectory();
	} catch {
		return false;
	}
}

function expandHome(value) {
	if (typeof value !== 'string') {
		return value;
	}
	if (value === '~') {
		return os.homedir();
	}
	if (value.startsWith(`~${path.sep}`)) {
		return path.resolve(os.homedir(), value.slice(2));
	}
	return value;
}

function unique(values) {
	return [...new Set(values.filter((value) => typeof value === 'string' && value.trim()))];
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
	DEFAULT_WP_CODEBOX_BIN,
	DEFAULT_WP_CLI_BIN,
	DEFAULT_CORE_MODULE,
	DEFAULT_RUNTIME_CORE_ENTRY,
	DEFAULT_RUNTIME_PLAYGROUND_ENTRY,
	assertWpCodeboxIdentityMatches,
	homeboySettings,
	installedExtensionSettingDefaults,
	resolveWpCodeboxIdentity,
	wpCodeboxCommand,
	wpCodeboxIdentityMismatchDiagnostics,
};
