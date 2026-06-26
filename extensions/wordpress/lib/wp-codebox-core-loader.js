'use strict';

/**
 * External dependencies
 */
const path = require('node:path');
const { existsSync, readdirSync } = require('node:fs');
const { homedir } = require('node:os');
const { pathToFileURL } = require('node:url');

/**
 * Internal dependencies
 */
const {
	resolveWpCodeboxIdentity,
} = require('./wp-codebox-resolver');

const DEFAULT_CODEBOX_CORE_MODULE = '@automattic/wp-codebox-core';
const RUNTIME_CORE_ENTRY = 'packages/runtime-core/dist/index.js';
const DEFAULT_CORE_PACKAGE_CANDIDATES = [DEFAULT_CODEBOX_CORE_MODULE];
const DEFAULT_RUNTIME_CORE_ENTRIES = [RUNTIME_CORE_ENTRY];

function coreModuleSpecifier(options = {}) {
	const identity = resolveWpCodeboxIdentity(options);
	const explicit = identity.coreModulePath || options.wpCodeboxCoreModule || options.coreModule || process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE || process.env.WP_CODEBOX_CORE_MODULE;
	if (!explicit) {
		return DEFAULT_CODEBOX_CORE_MODULE;
	}
	return normalizeCoreModuleSpecifier(explicit);
}

function normalizeCoreModuleSpecifier(specifier) {
	if (!specifier) {
		return specifier;
	}
	if (specifier.startsWith('file:') || specifier.startsWith('node:')) {
		return specifier;
	}
	if (isPathSpecifier(specifier)) {
		return pathToFileURL(path.resolve(specifier)).href;
	}
	return specifier;
}

function isPathSpecifier(specifier) {
	return specifier.startsWith('.') || path.isAbsolute(specifier) || specifier.startsWith('~') || specifier.includes('\\') || existsSync(path.resolve(specifier));
}

function coreModuleCandidates(options = {}) {
	const explicit = options.wpCodeboxCoreModule || options.coreModule || process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE || process.env.WP_CODEBOX_CORE_MODULE;
	if (explicit) {
		return [normalizeCoreModuleSpecifier(explicit)];
	}

	const usesCustomCandidateSearch = options.packageCandidates || options.runtimeCoreEntries || options.globalNodeModuleRoots;
	const identity = usesCustomCandidateSearch ? null : resolveWpCodeboxIdentity(options);
	if (identity?.coreModulePath && identity.coreModulePath !== DEFAULT_CODEBOX_CORE_MODULE) {
		return [normalizeCoreModuleSpecifier(identity.coreModulePath)];
	}

	const packageCandidates = options.packageCandidates || DEFAULT_CORE_PACKAGE_CANDIDATES;
	const candidates = [...packageCandidates];
	for (const candidate of setupCacheCoreModuleCandidates(options)) {
		if (existsSync(candidate) && !candidates.includes(candidate)) {
			candidates.push(candidate);
		}
	}

	for (const root of workspaceRoots(options)) {
		for (const repoPath of codeboxRepoCandidates(root)) {
			for (const entry of options.runtimeCoreEntries || DEFAULT_RUNTIME_CORE_ENTRIES) {
				const runtimeCore = path.resolve(repoPath, entry);
				if (existsSync(runtimeCore) && !candidates.includes(runtimeCore)) {
					candidates.push(runtimeCore);
				}
			}
		}
	}

	for (const root of globalNodeModuleRoots(options)) {
		for (const candidate of globalNodeModuleCoreCandidates(root, options)) {
			if (existsSync(candidate) && !candidates.includes(candidate)) {
				candidates.push(candidate);
			}
		}
	}

	return candidates.map(normalizeCoreModuleSpecifier);
}

function setupCacheCoreModuleCandidates(options = {}) {
	const installRoot = options.wpCodeboxInstallDir || process.env.HOMEBOY_WP_CODEBOX_INSTALL_DIR || path.resolve(homedir(), '.cache/homeboy/wp-codebox');
	const runtimeCoreEntries = options.runtimeCoreEntries || DEFAULT_RUNTIME_CORE_ENTRIES;
	const packageDistEntries = options.packageDistEntries || ['index.js'];
	const candidates = [];
	for (const entry of runtimeCoreEntries) {
		candidates.push(path.resolve(installRoot, 'source', entry));
		candidates.push(path.resolve(installRoot, 'release/wp-codebox-cli', entry));
	}
	for (const entry of packageDistEntries) {
		candidates.push(path.resolve(installRoot, 'source/node_modules/@automattic/wp-codebox-core/dist', entry));
		candidates.push(path.resolve(installRoot, 'release/wp-codebox-cli/node_modules/@automattic/wp-codebox-core/dist', entry));
	}
	return candidates;
}

function workspaceRoots(options = {}) {
	const repoRoot = path.resolve(__dirname, '..');
	const roots = [
		options.workspaceRoot,
		process.env.HOMEBOY_WORKSPACE_ROOT,
		process.env.HOMEBOY_DEVELOPER_WORKSPACE,
		path.dirname(repoRoot),
	];

	return [...new Set(roots.filter(Boolean))];
}

function globalNodeModuleRoots(options = {}) {
	if (options.includeGlobalNodeModuleRoots === false) {
		return [];
	}

	const configuredRoots = Array.isArray(options.globalNodeModuleRoots) ? options.globalNodeModuleRoots : [];
	const roots = [
		...configuredRoots,
		process.env.HOMEBOY_GLOBAL_NODE_MODULE_ROOT,
		path.resolve(path.dirname(process.execPath), '..', 'lib', 'node_modules'),
	];

	return [...new Set(roots.filter(Boolean))];
}

function globalNodeModuleCoreCandidates(root, options = {}) {
	const runtimeCoreEntries = options.runtimeCoreEntries || DEFAULT_RUNTIME_CORE_ENTRIES;
	const packageDistEntries = options.packageDistEntries || ['index.js'];
	const candidates = [];

	for (const entry of runtimeCoreEntries) {
		candidates.push(path.resolve(root, 'wp-codebox-workspace', entry));
	}
	for (const entry of packageDistEntries) {
		candidates.push(path.resolve(root, '@automattic', 'wp-codebox-core', 'dist', entry));
		candidates.push(path.resolve(root, 'wp-codebox-workspace', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', entry));
	}

	return candidates;
}

function codeboxRepoCandidates(root) {
	const exact = path.resolve(root, 'wp-codebox');
	const candidates = existsSync(exact) ? [exact] : [];

	try {
		const siblingWorktrees = readdirSync(root, { withFileTypes: true })
			.filter((entry) => entry.isDirectory() && entry.name.startsWith('wp-codebox@'))
			.map((entry) => path.resolve(root, entry.name))
			.sort();
		candidates.push(...siblingWorktrees);
	} catch {
		// A missing or unreadable workspace root simply contributes no candidates.
	}

	return candidates;
}

async function loadWpCodeboxCore(options = {}) {
	const errors = [];
	for (const specifier of coreModuleCandidates(options)) {
		try {
			return await import(specifier);
		} catch (error) {
			errors.push({ specifier, error });
		}
	}

	if (options.required) {
		const error = errors[0]?.error || new Error('WP Codebox core module is unavailable.');
		error.wpCodeboxCoreErrors = errors;
		throw error;
	}
	return null;
}

async function loadWpCodeboxCoreFunction(name, options = {}) {
	const result = await loadWpCodeboxCoreExport(name, options);
	return result ? result.value : null;
}

async function loadWpCodeboxCoreExport(name, options = {}) {
	const errors = [];
	for (const specifier of coreModuleCandidates(options)) {
		try {
			const core = await import(specifier);
			const value = core && core[name];
			if (typeof value !== 'function') {
				errors.push({ specifier, message: `missing ${name} export` });
				continue;
			}

			return { value, source: specifier };
		} catch (error) {
			errors.push({ specifier, error, message: error.message });
		}
	}

	if (options.required) {
		const error = new Error(`WP Codebox core export ${name} is unavailable.`);
		error.wpCodeboxCoreErrors = errors;
		throw error;
	}
	return null;
}

module.exports = {
	coreModuleSpecifier,
	coreModuleCandidates,
	loadWpCodeboxCore,
	loadWpCodeboxCoreExport,
	loadWpCodeboxCoreFunction,
	globalNodeModuleRoots,
	RUNTIME_CORE_ENTRY,
};
