/**
 * External dependencies
 */
import { existsSync, readdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const DEFAULT_CODEBOX_CORE_MODULE = '@automattic/wp-codebox-core';
const RUNTIME_CORE_ENTRY = 'packages/runtime-core/dist/index.js';

export async function loadCodeboxRecipeBuilder(requiredExport) {
	const configuredModule = process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE;
	const candidates = configuredModule ? [configuredModule] : discoverCodeboxCoreModuleCandidates();
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
		`Install/build a WP Codebox runtime-core module that exports ${requiredExport}.`,
		`Pass --setting wp_codebox_core_module=/path/to/wp-codebox/${RUNTIME_CORE_ENTRY}, or set HOMEBOY_WP_CODEBOX_CORE_MODULE to that built ESM entrypoint.`,
		`Fallback discovery also checks sibling wp-codebox checkouts for ${RUNTIME_CORE_ENTRY}.`,
		'This is separate from HOMEBOY_WP_CODEBOX_BIN / wp_codebox_bin, which only selects the wp-codebox CLI.',
		'Homeboy Extensions no longer falls back to bundled WP Codebox recipe builders because that stale local copy can drift from the Codebox recipe contract.',
		`Tried ${candidates.length} candidate(s):`,
		...errors.map((error) => `- ${error}`),
	].join('\n'));
}

function discoverCodeboxCoreModuleCandidates() {
	const candidates = [DEFAULT_CODEBOX_CORE_MODULE];
	const roots = workspaceRoots();

	for (const root of roots) {
		for (const repoPath of codeboxRepoCandidates(root)) {
			const runtimeCore = resolve(repoPath, RUNTIME_CORE_ENTRY);
			if (existsSync(runtimeCore) && !candidates.includes(runtimeCore)) {
				candidates.push(runtimeCore);
			}
		}
	}

	return candidates;
}

function workspaceRoots() {
	const scriptDir = dirname(fileURLToPath(import.meta.url));
	const repoRoot = resolve(scriptDir, '../../..');
	const roots = [
		process.env.HOMEBOY_WORKSPACE_ROOT,
		process.env.HOMEBOY_DEVELOPER_WORKSPACE,
		dirname(repoRoot),
	];

	return [...new Set(roots.filter(Boolean))];
}

function codeboxRepoCandidates(root) {
	const exact = resolve(root, 'wp-codebox');
	const candidates = existsSync(exact) ? [exact] : [];

	try {
		const siblingWorktrees = readdirSync(root, { withFileTypes: true })
			.filter((entry) => entry.isDirectory() && entry.name.startsWith('wp-codebox@'))
			.map((entry) => resolve(root, entry.name))
			.sort();
		candidates.push(...siblingWorktrees);
	} catch {
		// A missing or unreadable workspace root simply contributes no candidates.
	}

	return candidates;
}

async function importModule(specifier) {
	if (specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('file:')) {
		return import(specifier.startsWith('file:') ? specifier : pathToFileURL(resolve(specifier)).href);
	}
	return import(specifier);
}
