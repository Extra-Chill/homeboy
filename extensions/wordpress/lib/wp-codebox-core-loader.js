'use strict';

/**
 * External dependencies
 */
const path = require('node:path');
const { existsSync } = require('node:fs');
const { pathToFileURL } = require('node:url');

const DEFAULT_CODEBOX_CORE_MODULE = '@automattic/wp-codebox-core';
const DEFAULT_CORE_PACKAGE_CANDIDATES = [DEFAULT_CODEBOX_CORE_MODULE];

function coreModuleSpecifier(options = {}) {
	const explicit = options.wpCodeboxCoreModule || options.coreModule || process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE || process.env.WP_CODEBOX_CORE_MODULE;
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

	const packageCandidates = options.packageCandidates || DEFAULT_CORE_PACKAGE_CANDIDATES;
	return [...packageCandidates].map(normalizeCoreModuleSpecifier);
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
};
