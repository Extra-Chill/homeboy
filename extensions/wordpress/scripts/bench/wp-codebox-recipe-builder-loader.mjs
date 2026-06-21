/**
 * External dependencies
 */
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const {
	coreModuleCandidates,
	loadWpCodeboxCoreExport,
} = require('../../lib/wp-codebox-core-loader.js');

const RECIPE_BUILDER_MODULE_OPTIONS = {
	packageCandidates: [
		'@automattic/wp-codebox-core/recipe-builders',
		'wp-codebox-workspace/recipe-builders',
		'@automattic/wp-codebox-core',
	],
	packageDistEntries: ['recipe-builders.js', 'index.js'],
};

export async function loadCodeboxRecipeBuilder(requiredExport) {
	try {
		const result = await loadWpCodeboxCoreExport(requiredExport, { ...RECIPE_BUILDER_MODULE_OPTIONS, required: true });
		return { builder: result.value, source: result.source };
	} catch (error) {
		const candidates = coreModuleCandidates(RECIPE_BUILDER_MODULE_OPTIONS);
		const errors = (error.wpCodeboxCoreErrors || []).map(formatCoreLoaderError);

		throw new Error([
			`WP Codebox recipe builder export ${requiredExport} is unavailable.`,
			`Install/build a WP Codebox recipe-builder module that exports ${requiredExport}.`,
			'Use the public @automattic/wp-codebox-core/recipe-builders export or wp-codebox-workspace/recipe-builders.',
			`Pass --setting wp_codebox_core_module=@automattic/wp-codebox-core/recipe-builders, or set HOMEBOY_WP_CODEBOX_CORE_MODULE to a compatible recipe-builder module.`,
			'Fallback discovery only checks the configured WP Codebox install cache for packaged @automattic/wp-codebox-core entries.',
			'This is separate from HOMEBOY_WP_CODEBOX_BIN / wp_codebox_bin, which only selects the wp-codebox CLI.',
			'Homeboy Extensions no longer falls back to bundled WP Codebox recipe builders because that stale local copy can drift from the Codebox recipe contract.',
			`Tried ${candidates.length} candidate(s):`,
			...errors.map((error) => `- ${error}`),
		].join('\n'));
	}
}

function formatCoreLoaderError(error) {
	return `${error.specifier}: ${error.message}`;
}
