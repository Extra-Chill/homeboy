/**
 * External dependencies
 */
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const {
	coreModuleCandidates,
	loadWpCodeboxCoreExport,
	RUNTIME_CORE_ENTRY,
} = require('../../lib/wp-codebox-core-loader.js');

export async function loadCodeboxRecipeBuilder(requiredExport) {
	try {
		const result = await loadWpCodeboxCoreExport(requiredExport, { required: true });
		return { builder: result.value, source: result.source };
	} catch (error) {
		const candidates = coreModuleCandidates();
		const errors = (error.wpCodeboxCoreErrors || []).map(formatCoreLoaderError);

		throw new Error([
			`WP Codebox recipe builder export ${requiredExport} is unavailable.`,
			`Install/build a WP Codebox runtime-core module that exports ${requiredExport}.`,
			`Pass --setting wp_codebox_core_module=/path/to/wp-codebox/${RUNTIME_CORE_ENTRY}, or set HOMEBOY_WP_CODEBOX_CORE_MODULE to that built ESM entrypoint.`,
			`Fallback discovery also checks sibling wp-codebox checkouts for ${RUNTIME_CORE_ENTRY}.`,
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
