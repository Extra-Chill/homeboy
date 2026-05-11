#!/usr/bin/env node
/* eslint-disable no-console */
'use strict';

/**
 * External dependencies
 */
const fs = require('fs');
const path = require('path');

const SECRET_KEY_PATTERN = /(secret|token|password|passwd|authorization|cookie|nonce|api[_-]?key|access[_-]?key|private[_-]?key|github[_-]?token|openai[_-]?api[_-]?key)/i;

function usage() {
	console.error('Usage: build-replay-bundle.js --results <path> --scenario <id> --config <path> --output-dir <dir> [--update-results]');
}

function parseArgs(argv) {
	const args = {};
	for (let index = 0; index < argv.length; index += 1) {
		const arg = argv[index];
		if (arg === '--update-results') {
			args.updateResults = true;
			continue;
		}
		if (!arg.startsWith('--')) {
			throw new Error(`Unexpected argument: ${arg}`);
		}
		const key = arg.slice(2).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
		const value = argv[index + 1];
		if (!value || value.startsWith('--')) {
			throw new Error(`${arg} requires a value`);
		}
		args[key] = value;
		index += 1;
	}
	return args;
}

function readJson(filePath) {
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function safeId(value) {
	return String(value || 'scenario')
		.toLowerCase()
		.replace(/[^a-z0-9._-]+/g, '-')
		.replace(/^-+|-+$/g, '') || 'scenario';
}

function redact(value, key = '') {
	if (SECRET_KEY_PATTERN.test(key)) {
		return '[redacted]';
	}
	if (Array.isArray(value)) {
		return value.map((entry) => redact(entry));
	}
	if (value && typeof value === 'object') {
		return Object.fromEntries(Object.entries(value).map(([entryKey, entryValue]) => [entryKey, redact(entryValue, entryKey)]));
	}
	return value;
}

function compactObject(entries) {
	return Object.fromEntries(Object.entries(entries).filter(([, value]) => value !== undefined && value !== null && value !== ''));
}

function resolveReviewUrl(config, scenario) {
	const metadata = scenario.metadata && typeof scenario.metadata === 'object' ? scenario.metadata : {};
	return metadata.playground_review_url || metadata.review_url || config.playground_review_url || config.review_url || null;
}

function transcriptReferences(config, scenario) {
	const artifacts = scenario.artifacts && typeof scenario.artifacts === 'object' ? scenario.artifacts : {};
	const metadata = scenario.metadata && typeof scenario.metadata === 'object' ? scenario.metadata : {};
	const references = [];

	for (const [name, artifact] of Object.entries(artifacts)) {
		if (!/transcript|action.?log/i.test(name)) {
			continue;
		}
		if (artifact && typeof artifact === 'object' && artifact.path) {
			references.push({ name, path: artifact.path, label: artifact.label || name });
		}
	}

	for (const key of ['transcript_json', 'transcript_summary', 'action_log']) {
		if (metadata[key]) {
			references.push({ name: key, path: metadata[key] });
		}
	}

	if (config.transcript_dir) {
		references.push({ name: 'transcript_dir', path: config.transcript_dir });
	}

	return references;
}

function buildBundle(results, scenario, config, bundlePath) {
	const metadata = scenario.metadata && typeof scenario.metadata === 'object' ? scenario.metadata : {};
	const reviewUrl = resolveReviewUrl(config, scenario);

	return redact({
		schema_version: 1,
		generated_at: new Date().toISOString(),
		scenario_id: scenario.id,
		component_id: results.component_id || results.component,
		replay_bundle_path: bundlePath,
		scenario_manifest_snapshot: compactObject({
			id: scenario.id,
			label: metadata.label || scenario.label,
			source: scenario.source,
			file: scenario.file,
			manifest: metadata.scenario_manifest || metadata.manifest || metadata.scenario,
		}),
		initial_blueprint: config.playground_blueprint || {},
		prompt: config.prompt,
		runner_config: config,
		provider: config.provider || metadata.provider,
		model: config.model || metadata.model,
		seed: config.seed || metadata.seed,
		transcripts: transcriptReferences(config, scenario),
		grader: compactObject({
			job_status: metadata.job_status,
			completion_outcome: metadata.completion_outcome,
			metrics: scenario.metrics,
			result_metadata: metadata,
		}),
		logs: compactObject({
			stage_log: metadata.stage_log,
			error: metadata.error,
		}),
		final_state: {
			available: false,
			reason: 'The current Playground PHP bench runner exits after scenario execution and does not export a restorable final site state.',
		},
		playground_review: reviewUrl ? {
			available: true,
			url: reviewUrl,
		} : {
			available: false,
			reason: 'No hosted final-state artifact or caller-supplied Playground review URL was available for this scenario.',
		},
		redaction: {
			applied: true,
			key_pattern: SECRET_KEY_PATTERN.source,
		},
	});
}

function attachBundle(results, scenarioId, relativeBundlePath, reviewUrl) {
	return {
		...results,
		scenarios: (results.scenarios || []).map((scenario) => {
			if (scenario.id !== scenarioId) {
				return scenario;
			}
			return {
				...scenario,
				artifacts: {
					...(scenario.artifacts || {}),
					replay_bundle: {
						path: relativeBundlePath,
						kind: 'json',
						label: 'Replay bundle',
					},
				},
				metadata: {
					...(scenario.metadata || {}),
					replay_bundle_path: relativeBundlePath,
					playground_review: reviewUrl ? {
						available: true,
						url: reviewUrl,
					} : {
						available: false,
						reason: 'Final-state Playground URLs need a hosted state export or caller-supplied review URL.',
					},
				},
			};
		}),
	};
}

function main() {
	const args = parseArgs(process.argv.slice(2));
	for (const required of ['results', 'scenario', 'config', 'outputDir']) {
		if (!args[required]) {
			usage();
			throw new Error(`Missing --${required.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`)}`);
		}
	}

	const results = readJson(args.results);
	const config = readJson(args.config);
	const scenario = (results.scenarios || []).find((entry) => entry.id === args.scenario);
	if (!scenario) {
		throw new Error(`Scenario not found in results: ${args.scenario}`);
	}

	fs.mkdirSync(args.outputDir, { recursive: true });
	const filename = `${safeId(args.scenario)}-replay-bundle.json`;
	const bundlePath = path.join(args.outputDir, filename);
	const relativeBundlePath = path.relative(path.dirname(args.results), bundlePath) || filename;
	const bundle = buildBundle(results, scenario, config, relativeBundlePath);
	fs.writeFileSync(bundlePath, `${JSON.stringify(bundle, null, 2)}\n`);

	if (args.updateResults) {
		const updated = attachBundle(results, args.scenario, relativeBundlePath, resolveReviewUrl(config, scenario));
		fs.writeFileSync(args.results, `${JSON.stringify(updated, null, 2)}\n`);
	}

	console.log(bundlePath);
}

try {
	main();
} catch (error) {
	console.error(`ERROR: ${error.message}`);
	process.exit(1);
}
