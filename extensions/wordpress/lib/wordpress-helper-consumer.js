'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/**
 * Internal dependencies
 */
const { getWordPressHelperManifest } = require('./helper-manifest');

const MANIFEST_ENV = 'HOMEBOY_WORDPRESS_HELPER_MANIFEST';
const ROOT_ENV = 'HOMEBOY_WORDPRESS_EXTENSION_ROOT';
const INSTALL_COMMAND = 'homeboy extension install https://github.com/Extra-Chill/homeboy --id wordpress';
const { helperEnv } = getWordPressHelperManifest();

function resolveManifest(options) {
	if (options.manifestPath || process.env[MANIFEST_ENV]) {
		return { path: path.resolve(options.manifestPath || process.env[MANIFEST_ENV]), source: options.manifestPath ? 'explicit' : MANIFEST_ENV };
	}
	if (process.env[ROOT_ENV]) {
		return { path: path.resolve(process.env[ROOT_ENV], 'lib', 'helper-manifest.js'), source: ROOT_ENV };
	}
	const home = os.homedir();
	const configRoot = process.env.HOMEBOY_CONFIG_ROOT;
	const defaultRoot = process.platform === 'win32'
		? path.join(process.env.APPDATA || home, 'homeboy')
		: path.join(home, '.config', 'homeboy');
	const root = configRoot?.trim() ? configRoot.replace(/^~(?=$|[/\\])/, () => home) : defaultRoot;
	return { path: path.resolve(root, 'extensions', 'wordpress', 'lib', 'helper-manifest.js'), source: 'installed' };
}

function setupHint({ stale = false, envVar } = {}) {
	return `${stale ? 'Refresh a managed install with' : 'Install WordPress helpers with'}: ${INSTALL_COMMAND}${stale ? ' --replace' : ''}. `
		+ `For a different checkout, set ${MANIFEST_ENV} to its lib/helper-manifest.js or set ${ROOT_ENV}.`
		+ (envVar ? ` To override just this helper, set ${envVar} to its module path.` : '');
}

function loadWordPressHelperManifest(options = {}) {
	const resolved = resolveManifest(options);
	const manifestPath = resolved.path;
	const missing = (reason) => ({ ...resolved, manifest: null, found: false, reason });
	if (!fs.existsSync(manifestPath)) {
		return missing(`WordPress helper manifest does not exist at ${manifestPath} (${resolved.source}). ${setupHint()}`);
	}

	let manifest;
	try {
		const helperModule = require(manifestPath);
		manifest = typeof helperModule.getWordPressHelperManifest === 'function'
			? helperModule.getWordPressHelperManifest()
			: helperModule.WORDPRESS_HELPER_MANIFEST;
	} catch (error) {
		throw new Error(`Could not load WordPress helper manifest at ${manifestPath} (${resolved.source}): ${error.message}. ${setupHint({ stale: true })}`, { cause: error });
	}

	if (!manifest || typeof manifest !== 'object') {
		return missing(`WordPress helper manifest module did not export a manifest: ${manifestPath}. ${setupHint({ stale: true })}`);
	}

	return { ...resolved, manifest, found: true, reason: '' };
}

function resolveHelper(name, options) {
	if (typeof name !== 'string' || name.trim() === '') {
		throw new TypeError('helper name must be a non-empty string');
	}
	const envVar = options.envVar || (Object.hasOwn(helperEnv, name) ? helperEnv[name] : '');
	const explicit = options.override || (envVar && process.env[envVar]);
	if (explicit) {
		return { path: explicit, source: options.override ? 'explicit' : envVar, envVar };
	}

	const { manifest, path: manifestPath, reason, source } = loadWordPressHelperManifest(options);
	const helperPath = manifest?.helpers && Object.hasOwn(manifest.helpers, name) ? manifest.helpers[name] : '';
	if (typeof helperPath === 'string' && helperPath) {
		return { path: helperPath, source, envVar, fromManifest: true };
	}

	return {
		path: '',
		source,
		envVar,
		reason: manifest
			? `WordPress helper "${name}" could not be resolved: ${manifestPath} (${source}) does not declare it; the install may be stale. ${setupHint({ stale: true, envVar })}`
			: `${reason}${envVar ? ` To override just this helper, set ${envVar} to its module path.` : ''}`,
	};
}

function wordpressHelperPath(name, options = {}) {
	return resolveHelper(name, options).path;
}

function wordpressLibHelperPath(fileName, options = {}) {
	if (typeof fileName !== 'string' || fileName.trim() === '' || fileName.includes('/') || fileName.includes('\\')) {
		throw new TypeError('fileName must be a helper file name, not a path');
	}

	const explicit = options.override || (options.envVar ? process.env[options.envVar] : '');
	if (explicit) {
		return explicit;
	}

	const { manifest } = loadWordPressHelperManifest(options);
	return manifest?.extensionRoot ? path.join(manifest.extensionRoot, 'lib', fileName) : '';
}

function loadResolvedHelper(resolved, options) {
	const { path: resolvedPath, source, envVar, fromManifest } = resolved;
	if (!resolvedPath || !fs.existsSync(resolvedPath)) {
		const reason = resolved.reason || (resolvedPath
			? `WordPress helper does not exist at ${resolvedPath} (${source}). ${setupHint({ stale: fromManifest, envVar })}`
			: 'WordPress helper path could not be resolved');
		if (options.required) {
			throw new Error(reason);
		}
		return { path: resolvedPath, source, module: null, found: false, reason };
	}

	try {
		return { path: resolvedPath, source, module: require(path.resolve(resolvedPath)), found: true, reason: '' };
	} catch (error) {
		throw new Error(`Could not load WordPress helper at ${resolvedPath} (${source}): ${error.message}`, { cause: error });
	}
}

function loadWordPressHelper(name, options = {}) {
	return loadResolvedHelper(resolveHelper(name, options), options);
}

function loadWordPressLibHelper(fileName, options = {}) {
	const helperPath = wordpressLibHelperPath(fileName, options);
	return loadResolvedHelper({ path: helperPath, source: 'legacy lib lookup' }, options);
}

module.exports = {
	loadWordPressHelper,
	loadWordPressHelperManifest,
	loadWordPressLibHelper,
	wordpressHelperPath,
	wordpressLibHelperPath,
};
