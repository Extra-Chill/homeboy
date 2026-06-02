#!/usr/bin/env node
import { createRequire } from 'node:module';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { readFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';

const input = JSON.parse(readFileSync(0, 'utf8'));
const { buildWordPressBenchRecipe } = await loadWpCodeboxCore(input.wpCodeboxBin);

if (typeof buildWordPressBenchRecipe !== 'function') {
	throw new Error('WP Codebox core does not export buildWordPressBenchRecipe(). Update wp-codebox after chubes4/wp-codebox#487.');
}

const recipe = buildWordPressBenchRecipe(input.options);
if (input.options?.pluginRuntime && typeof input.options.pluginRuntime === 'object' && !Array.isArray(input.options.pluginRuntime)) {
	recipe.inputs = recipe.inputs ?? {};
	recipe.inputs.pluginRuntime = input.options.pluginRuntime;
}

process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);

async function loadWpCodeboxCore(wpCodeboxBin) {
	const candidates = [];
	if (process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE) {
		candidates.push(process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE);
	}
	candidates.push('@chubes4/wp-codebox-core', 'wp-codebox-workspace/core');

	const cliEntrypoint = resolveWpCodeboxCliEntrypoint(wpCodeboxBin);
	if (cliEntrypoint) {
		const requireFromCli = createRequire(pathToFileURL(cliEntrypoint));
		try {
			candidates.push(requireFromCli.resolve('@chubes4/wp-codebox-core'));
		} catch {
			// The development checkout exposes runtime-core as a sibling package.
		}
		const cliDir = dirname(cliEntrypoint);
		candidates.push(
			join(cliDir, '..', '..', 'runtime-core', 'dist', 'index.js'),
			join(cliDir, '..', 'node_modules', '@chubes4', 'wp-codebox-core', 'dist', 'index.js')
		);
	}

	const errors = [];
	for (const candidate of unique(candidates)) {
		try {
			return await importModule(candidate);
		} catch (error) {
			errors.push(`${candidate}: ${error.message}`);
		}
	}

	throw new Error(`Unable to load WP Codebox core recipe builders. Tried:\n${errors.map((error) => `- ${error}`).join('\n')}`);
}

async function importModule(specifier) {
	if (specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('file:')) {
		return import(specifier.startsWith('file:') ? specifier : pathToFileURL(resolve(specifier)).href);
	}
	return import(specifier);
}

function resolveWpCodeboxCliEntrypoint(wpCodeboxBin) {
	if (!wpCodeboxBin || !isAbsolute(wpCodeboxBin)) {
		return '';
	}

	const binText = readFileSync(wpCodeboxBin, 'utf8');
	const nodeExecMatch = binText.match(/exec\s+node\s+([^\s"']+)/);
	if (nodeExecMatch) {
		return isAbsolute(nodeExecMatch[1]) ? nodeExecMatch[1] : resolve(dirname(wpCodeboxBin), nodeExecMatch[1]);
	}

	return wpCodeboxBin;
}

function unique(values) {
	return [...new Set(values.filter(Boolean))];
}
