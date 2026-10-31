#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);

const {
	buildWpCodeboxFixtureWorkloadMatrixRecipe,
	collectFixtureWorkloadMatrixRunResults,
	createFixtureWorkloadMatrix,
	normalizeFixtureWorkloadMatrixResult,
	writeFixtureWorkloadMatrixArtifacts,
	writeFixtureWorkloadMatrixResultArtifacts,
} = require('../lib/fixture-workload-matrix');
const { runWpCodeboxRecipe } = require('../lib/wp-codebox-recipe-helper');

const DEFAULT_RUN_BATCH_SIZE = 10;

async function main() {
	const options = parseArgs(process.argv.slice(2));
	if (options.help || !options.fixtureRoot) {
		printHelp();
		process.exit(options.help ? 0 : 1);
	}

	const outputDirectory = path.resolve(options.outputDirectory || path.join(process.cwd(), 'artifacts', 'fixture-workload-matrix'));
	const matrix = createFixtureWorkloadMatrix({
		id: options.id || `fixture-workload-matrix-${Date.now()}`,
		fixture_root: path.resolve(options.fixtureRoot),
		entrypoint: options.entrypoint || 'index.html',
		maxDepth: options.maxDepth,
		batchSize: options.batchSize || (options.run ? DEFAULT_RUN_BATCH_SIZE : undefined),
	});
	const written = writeFixtureWorkloadMatrixArtifacts({ outputDirectory, matrix });
	const recipe = buildWpCodeboxFixtureWorkloadMatrixRecipe({
		matrix,
		artifactsDirectory: outputDirectory,
		playgroundArtifactsDirectory: options.playgroundArtifactsDirectory,
		wordpressVersion: options.wordpressVersion,
		extra_plugins: parseJsonOption(options.extra_plugins, []),
		pluginActivations: parseJsonOption(options.pluginActivations, []),
		workloadStep: parseJsonOption(options.workloadStep, null),
		argsTemplate: options.commandArgs ? [options.commandArgs] : undefined,
		command: options.command,
	});
	const recipeFile = path.join(outputDirectory, 'wp-codebox-fixture-workload-matrix-recipe.json');
	fs.writeFileSync(recipeFile, `${JSON.stringify(recipe, null, 2)}\n`);

	let runtime = null;
	let runtimeError = null;
	let collectedResult = written.result;
	if (options.run) {
		const batchRuns = [];
		const batchResults = [];
		for (const batch of matrix.batches) {
			const batchFixtures = matrix.fixtures.filter((fixture) => batch.fixture_ids.includes(fixture.id));
			const batchMatrix = createFixtureWorkloadMatrix({
				id: `${matrix.id}-${batch.id}`,
				fixture_root: matrix.fixture_root,
				entrypoint: matrix.entrypoint,
				fixtures: batchFixtures,
			});
			const batchRecipe = buildWpCodeboxFixtureWorkloadMatrixRecipe({
				matrix: batchMatrix,
				artifactsDirectory: outputDirectory,
				playgroundArtifactsDirectory: options.playgroundArtifactsDirectory,
				wordpressVersion: options.wordpressVersion,
				extra_plugins: parseJsonOption(options.extra_plugins, []),
				pluginActivations: parseJsonOption(options.pluginActivations, []),
				workloadStep: parseJsonOption(options.workloadStep, null),
				argsTemplate: options.commandArgs ? [options.commandArgs] : undefined,
				command: options.command,
			});
			const batchRecipeFile = path.join(outputDirectory, `wp-codebox-fixture-workload-matrix-${batch.id}.json`);
			const outputFile = path.join(outputDirectory, `wp-codebox-output-${batch.id}.json`);
			fs.writeFileSync(batchRecipeFile, `${JSON.stringify(batchRecipe, null, 2)}\n`);

			let batchRuntime = null;
			let batchError = null;
			try {
				batchRuntime = await runWpCodeboxRecipe({ recipeFile: batchRecipeFile, artifactsDir: outputDirectory, outputFile, wpCodeboxBin: options.wpCodeboxBin });
			} catch (error) {
				batchError = error;
				batchRuntime = { exitCode: error?.code ?? 1, outputFile, json: parseJsonText(error?.stdout) };
				if (!runtimeError) {
					runtimeError = error;
				}
			}

			batchRuns.push({
				batch: batch.id,
				fixture_count: batchFixtures.length,
				recipe_file: batchRecipeFile,
				output_file: outputFile,
				exit_code: batchRuntime?.exitCode ?? 0,
				error: batchError ? batchError.message : '',
			});
			batchResults.push(collectFixtureWorkloadMatrixRunResults({
				matrix: batchMatrix,
				outputDirectory,
				outputFile,
				codeboxOutput: batchRuntime?.json,
				codeboxError: batchError,
			}));
		}
		collectedResult = normalizeFixtureWorkloadMatrixResult({
			matrix,
			results: batchResults.flatMap((result) => result.fixtures),
		});
		runtime = {
			exitCode: runtimeError ? (batchRuns.find((batch) => batch.exit_code)?.exit_code || 1) : 0,
			batchSize: Number(options.batchSize || DEFAULT_RUN_BATCH_SIZE),
			batches: batchRuns,
		};
		writeFixtureWorkloadMatrixResultArtifacts({ outputDirectory, matrix, result: collectedResult });
	}

	const summary = {
		schema: 'homeboy/fixture-workload-matrix-cli-run/v1',
		matrix_id: matrix.id,
		fixture_root: matrix.fixture_root,
		fixture_count: matrix.count,
		batch_count: matrix.batch_count,
		output_directory: outputDirectory,
		recipe_file: recipeFile,
		artifact_refs: written.artifact_refs,
		result_file: path.join(outputDirectory, matrix.artifacts.result),
		result_summary: collectedResult.summary,
		runtime: runtime ? runtimeSummary(runtime, runtimeError) : null,
	};
	fs.writeFileSync(path.join(outputDirectory, 'cli-run.json'), `${JSON.stringify(summary, null, 2)}\n`);
	process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
	if (runtimeError) {
		process.exitCode = runtime.exitCode || 1;
	}
}

function runtimeSummary(runtime, runtimeError) {
	return {
		exit_code: runtime.exitCode,
		...(runtime.outputFile ? { output_file: runtime.outputFile } : {}),
		...(runtime.batchSize ? { batch_size: runtime.batchSize } : {}),
		...(runtime.batches ? { batches: runtime.batches } : {}),
		error: runtimeError ? runtimeError.message : '',
	};
}

function parseJsonText(text) {
	try {
		return text ? JSON.parse(text) : null;
	} catch {
		return null;
	}
}

function parseJsonOption(value, fallback) {
	if (!value) {
		return fallback;
	}
	try {
		return JSON.parse(value);
	} catch (error) {
		throw new Error(`Invalid JSON option: ${error.message}`);
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
	process.stdout.write(`Usage: node scripts/fixture-workload-matrix.mjs --fixture-root <path> --command-args <template> [options]\n\nOptions:\n  --output-directory <path>          Directory for matrix artifacts and WP Codebox recipe.\n  --entrypoint <file>                Fixture entry file name. Default: index.html.\n  --max-depth <number>               Discovery depth below fixture root. Default: 2.\n  --batch-size <number>              Matrix batch size. Default: all fixtures in one batch, ${DEFAULT_RUN_BATCH_SIZE} with --run.\n  --wordpress-version <ver>          WP Codebox WordPress runtime version. Default: latest.\n  --playground-artifacts-directory   Mounted artifact directory inside WP Codebox.\n  --extra-plugins <json>             Extra plugin input array passed through to WP Codebox.\n  --plugin-activations <json>        Plugin activation inputs as strings or step objects.\n  --workload-step <json>             Complete workload step template with {{ fixture_id }} and {{ artifact_path }} placeholders.\n  --command <name>                   Workload command. Default: wordpress.wp-cli.\n  --command-args <template>          Workload command args template.\n  --wp-codebox-bin <path>            WP Codebox CLI binary for --run.\n  --run                              Execute the generated recipe with WP Codebox.\n`);
}

main().catch((error) => {
	process.stderr.write(`${error && error.stack ? error.stack : String(error)}\n`);
	process.exit(1);
});
