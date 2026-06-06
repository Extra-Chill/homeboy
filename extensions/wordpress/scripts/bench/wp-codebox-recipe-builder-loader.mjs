/**
 * External dependencies
 */
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const DEFAULT_CODEBOX_CORE_MODULE = '@automattic/wp-codebox-core';

export async function loadCodeboxRecipeBuilder(requiredExport) {
	const configuredModule = process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE;
	const candidates = configuredModule ? [configuredModule] : [DEFAULT_CODEBOX_CORE_MODULE];
	const errors = [];

	for (const candidate of candidates) {
		try {
			const module = await importModule(candidate);
			if (typeof module[requiredExport] !== 'function') {
				errors.push(`${candidate}: missing ${requiredExport} export`);
				continue;
			}

			return { builder: module[requiredExport], source: candidate };
		} catch (error) {
			errors.push(`${candidate}: ${error.message}`);
		}
	}

	throw new Error([
		`WP Codebox recipe builder export ${requiredExport} is unavailable.`,
		`Install a WP Codebox core module that exports ${requiredExport}, or set HOMEBOY_WP_CODEBOX_CORE_MODULE to its built ESM entrypoint.`,
		'Homeboy Extensions no longer falls back to bundled WP Codebox recipe builders because that stale local copy can drift from the Codebox recipe contract.',
		...errors.map((error) => `- ${error}`),
	].join('\n'));
}

async function importModule(specifier) {
	if (specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('file:')) {
		return import(specifier.startsWith('file:') ? specifier : pathToFileURL(resolve(specifier)).href);
	}
	return import(specifier);
}
