'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

function resolveManifestPath(options = {}) {
	return options.manifestPath || process.env.HOMEBOY_WORDPRESS_HELPER_MANIFEST || '';
}

function missingHandle(resolvedPath, reason) {
	return {
		path: resolvedPath || '',
		module: null,
		found: false,
		reason,
	};
}

function loadWordPressHelperManifest(options = {}) {
	const manifestPath = resolveManifestPath(options);
	if (!manifestPath) {
		return {
			path: '',
			manifest: null,
			found: false,
			reason: 'HOMEBOY_WORDPRESS_HELPER_MANIFEST is not set',
		};
	}

	if (!fs.existsSync(manifestPath)) {
		return {
			path: manifestPath,
			manifest: null,
			found: false,
			reason: `WordPress helper manifest does not exist: ${manifestPath}`,
		};
	}

	const helperModule = require(manifestPath);
	const manifest = typeof helperModule.getWordPressHelperManifest === 'function'
		? helperModule.getWordPressHelperManifest()
		: helperModule.WORDPRESS_HELPER_MANIFEST;

	if (!manifest || typeof manifest !== 'object') {
		return {
			path: manifestPath,
			manifest: null,
			found: false,
			reason: `WordPress helper manifest module did not export a manifest: ${manifestPath}`,
		};
	}

	return {
		path: manifestPath,
		manifest,
		found: true,
		reason: '',
	};
}

function wordpressHelperPath(name, options = {}) {
	if (typeof name !== 'string' || name.trim() === '') {
		throw new TypeError('helper name must be a non-empty string');
	}

	const explicit = options.override || (options.envVar ? process.env[options.envVar] : '');
	if (explicit) {
		return explicit;
	}

	const { manifest } = loadWordPressHelperManifest(options);
	return manifest?.helpers?.[name] || '';
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

function loadResolvedHelper(resolvedPath, options = {}) {
	if (!resolvedPath) {
		const handle = missingHandle('', 'WordPress helper path could not be resolved');
		if (options.required) {
			throw new Error(handle.reason);
		}
		return handle;
	}

	if (!fs.existsSync(resolvedPath)) {
		const handle = missingHandle(resolvedPath, `WordPress helper does not exist: ${resolvedPath}`);
		if (options.required) {
			throw new Error(handle.reason);
		}
		return handle;
	}

	return {
		path: resolvedPath,
		module: require(resolvedPath),
		found: true,
		reason: '',
	};
}

function loadWordPressHelper(name, options = {}) {
	return loadResolvedHelper(wordpressHelperPath(name, options), options);
}

function loadWordPressLibHelper(fileName, options = {}) {
	return loadResolvedHelper(wordpressLibHelperPath(fileName, options), options);
}

module.exports = {
	loadWordPressHelper,
	loadWordPressHelperManifest,
	loadWordPressLibHelper,
	wordpressHelperPath,
	wordpressLibHelperPath,
};
