#!/usr/bin/env node

/**
 * External dependencies
 */
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);

/**
 * Internal dependencies
 */
const {
	buildStaticSiteFixtureMatrixRecipe,
	collectStaticSiteFixtureMatrixRunResults,
	createStaticSiteFixtureMatrix,
	writeStaticSiteFixtureMatrixResultArtifacts,
	writeStaticSiteFixtureMatrixArtifacts,
} = require('../lib/static-site-fixture-matrix');
const { runWpCodeboxRecipe } = require('../lib/wp-codebox-recipe-helper');

async function main() {
	const options = parseArgs(process.argv.slice(2));
	if (options.help || !options.fixtureRoot) {
		printHelp();
		process.exit(options.help ? 0 : 1);
	}

	const outputDirectory = path.resolve(options.outputDirectory || path.join(process.cwd(), 'artifacts', 'static-site-fixture-matrix'));
	const matrix = createStaticSiteFixtureMatrix({
		id: options.id || `static-site-fixture-matrix-${Date.now()}`,
		fixture_root: path.resolve(options.fixtureRoot),
		entrypoint: options.entrypoint || 'index.html',
		maxDepth: options.maxDepth,
	});
	const written = writeStaticSiteFixtureMatrixArtifacts({
		outputDirectory,
		matrix,
	});
	const recipe = buildStaticSiteFixtureMatrixRecipe({
		matrix,
		artifactsDirectory: outputDirectory,
		wordpressVersion: options.wordpressVersion,
	});
	const recipeFile = path.join(outputDirectory, 'wp-codebox-static-site-fixture-matrix-recipe.json');
	fs.writeFileSync(recipeFile, `${JSON.stringify(recipe, null, 2)}\n`);

	let runtime = null;
	let runtimeError = null;
	let collectedResult = written.result;
	if (options.run) {
		const outputFile = path.join(outputDirectory, 'wp-codebox-output.json');
		try {
			runtime = await runWpCodeboxRecipe({
				recipeFile,
				artifactsDir: outputDirectory,
				outputFile,
				wpCodeboxBin: options.wpCodeboxBin,
			});
		} catch (error) {
			runtimeError = error;
			runtime = {
				exitCode: error?.code ?? 1,
				outputFile,
				json: parseJsonText(error?.stdout),
			};
		}
		collectedResult = collectStaticSiteFixtureMatrixRunResults({
			matrix,
			outputDirectory,
			outputFile,
			codeboxOutput: runtime?.json,
			codeboxError: runtimeError,
		});
		writeStaticSiteFixtureMatrixResultArtifacts({
			outputDirectory,
			matrix,
			result: collectedResult,
		});
	}

	const summary = {
		schema: 'homeboy/static-site-fixture-matrix-cli-run/v1',
		matrix_id: matrix.id,
		fixture_root: matrix.fixture_root,
		fixture_count: matrix.count,
		output_directory: outputDirectory,
		recipe_file: recipeFile,
		artifact_refs: written.artifact_refs,
		result_file: path.join(outputDirectory, 'static-site-fixture-matrix-result.json'),
		result_summary: collectedResult.summary,
		runtime: runtime ? { exit_code: runtime.exitCode, output_file: runtime.outputFile, error: runtimeError ? runtimeError.message : '' } : null,
	};
	fs.writeFileSync(path.join(outputDirectory, 'cli-run.json'), `${JSON.stringify(summary, null, 2)}\n`);
	process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
	if (runtimeError) {
		process.exitCode = runtime.exitCode || 1;
	}
}

function parseJsonText(text) {
	try {
		return text ? JSON.parse(text) : null;
	} catch {
		return null;
	}
}

function parseArgs(args) {
	const options = {};
	for (let index = 0; index < args.length; index += 1) {
		const arg = args[index];
		if (arg === '--help' || arg === '-h') {
			options.help = true;
			continue;
		}
		if (arg === '--run') {
			options.run = true;
			continue;
		}
		if (arg.startsWith('--')) {
			const [rawKey, rawValue] = arg.slice(2).split('=');
			const key = camelCase(rawKey);
			const value = rawValue === undefined ? args[index + 1] : rawValue;
			if (rawValue === undefined) {
				index += 1;
			}
			options[key] = value;
			continue;
		}
		if (!options.fixtureRoot) {
			options.fixtureRoot = arg;
		}
	}
	return options;
}

function camelCase(value) {
	return value.replace(/-([a-z])/g, (_match, letter) => letter.toUpperCase());
}

function printHelp() {
	process.stdout.write(`Usage: node scripts/static-site-fixture-matrix.mjs --fixture-root <path> [options]\n\nOptions:\n  --output-directory <path>   Directory for matrix artifacts and WP Codebox recipe.\n  --entrypoint <file>         Fixture entry file name. Default: index.html.\n  --max-depth <number>        Discovery depth below fixture root. Default: 2.\n  --wordpress-version <ver>   WP Codebox WordPress runtime version. Default: latest.\n  --wp-codebox-bin <path>     WP Codebox CLI binary for --run.\n  --run                       Execute the generated recipe with WP Codebox.\n`);
}

main().catch((error) => {
	process.stderr.write(`${error && error.stack ? error.stack : String(error)}\n`);
	process.exit(1);
});
