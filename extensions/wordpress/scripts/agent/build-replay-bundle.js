#!/usr/bin/env node
/* eslint-disable no-console */
'use strict';

/**
 * External dependencies
 */
const fs = require('fs');
const path = require('path');

/**
 * Internal dependencies
 */
const {
	SECRET_KEY_PATTERN,
	safeId,
	redact,
	compactObject,
	sha256Buffer,
	sha256Json,
	artifactReferences,
	hashArtifactReferences,
	writeEpisodeJsonl,
	buildSealedEnvelope,
} = require('./replay-envelope');

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

function resolveReviewUrl(config, scenario) {
	const metadata = scenario.metadata && typeof scenario.metadata === 'object' ? scenario.metadata : {};
	return metadata.playground_review_url || metadata.review_url || config.playground_review_url || config.review_url || null;
}

function transcriptReferences(config, scenario) {
	const artifacts = scenario.artifacts && typeof scenario.artifacts === 'object' ? scenario.artifacts : {};
	const metadata = scenario.metadata && typeof scenario.metadata === 'object' ? scenario.metadata : {};
	const transcriptArtifacts = metadata.transcript_artifacts && typeof metadata.transcript_artifacts === 'object' ? metadata.transcript_artifacts : {};
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
	if (transcriptArtifacts.json) {
		references.push({ name: 'transcript_json', path: transcriptArtifacts.json });
	}
	if (transcriptArtifacts.summary) {
		references.push({ name: 'transcript_summary', path: transcriptArtifacts.summary });
	}

	if (config.transcript_dir) {
		references.push({ name: 'transcript_dir', path: config.transcript_dir });
	}

	return references;
}

function objectValue(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function firstObject(...values) {
	return values.find((value) => value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length > 0) || {};
}

function firstDefined(...values) {
	return values.find((value) => value !== undefined && value !== null && value !== '');
}

function canonicalGrade(value) {
	if (!value || typeof value !== 'object' || Array.isArray(value) || typeof value.score !== 'number') {
		return undefined;
	}
	return value;
}

function canonicalStatus(statusConfig, metadata, success) {
	const allowedOutcomes = new Set(['passed', 'failed', 'errored']);
	const allowedFailureClasses = new Set(['none', 'runtime_failure', 'agent_failure', 'grader_failure', 'task_failure']);
	const configuredOutcome = firstDefined(statusConfig.outcome, metadata.status?.outcome);
	const configuredFailureClass = firstDefined(statusConfig.failure_class, metadata.failure_class, metadata.status?.failure_class);
	let outcome;
	let failureClass;

	if (allowedOutcomes.has(configuredOutcome)) {
		outcome = configuredOutcome;
	} else if (success === true) {
		outcome = 'passed';
	} else if (success === false) {
		outcome = 'failed';
	}

	if (allowedFailureClasses.has(configuredFailureClass)) {
		failureClass = configuredFailureClass;
	} else if (success === true) {
		failureClass = 'none';
	} else if (success === false) {
		failureClass = 'task_failure';
	}

	return outcome && failureClass ? { outcome, failure_class: failureClass } : undefined;
}

function buildWpGymProjection(scenario, config, metadata, evalArtifact) {
	const configEval = objectValue(config.wp_gym_eval);
	const metadataEval = objectValue(metadata.wp_gym_eval);
	if (Object.keys(configEval).length === 0 && Object.keys(metadataEval).length === 0) {
		return undefined;
	}

	const manifest = objectValue(metadata.scenario_manifest || metadata.manifest || metadata.scenario);
	const scenarioConfig = firstObject(metadataEval.scenario, configEval.scenario, manifest.wp_gym?.scenario, manifest.scenario);
	const taskSetConfig = firstObject(metadataEval.task_set, configEval.task_set, manifest.wp_gym?.task_set, manifest.task_set);
	const graderConfig = firstObject(metadataEval.grader, configEval.grader, manifest.wp_gym?.grader);
	const statusConfig = firstObject(metadataEval.status, configEval.status, manifest.wp_gym?.status);
	const fingerprints = objectValue(metadata.fingerprints);
	const evalHashes = objectValue(evalArtifact.hashes);
	const grade = canonicalGrade(firstObject(graderConfig.grade, evalArtifact.grade, metadata.grade));
	const reward = firstDefined(graderConfig.reward, metadata.reward, metadata.score, grade?.reward, grade?.score);
	const failureReasons = firstDefined(graderConfig.failure_reasons, evalArtifact.failure_reasons, metadata.failure_reasons, []);
	const success = firstDefined(graderConfig.success, metadata.success, metadata.success_status === 'success' ? true : undefined, grade?.score !== undefined && grade?.max_score !== undefined ? grade.score >= grade.max_score : undefined);
	const status = canonicalStatus(statusConfig, metadata, success);
	const taskFamily = firstDefined(scenarioConfig.task_family, metadataEval.task_family, configEval.task_family, metadata.task_family, config.task_family, manifest.task_family);

	return compactObject({
		scenario: compactObject({
			id: firstDefined(scenarioConfig.id, metadataEval.scenario_id, configEval.scenario_id, metadata.task_id, config.task_id, config.workload_id, scenario.id),
			label: firstDefined(scenarioConfig.label, metadataEval.scenario_label, configEval.scenario_label, metadata.task_label, config.task_label, config.workload_label, scenario.label),
			task_family: taskFamily,
			prompt_sha256: firstDefined(scenarioConfig.prompt_sha256, fingerprints.prompt?.sha256, evalHashes.prompt?.sha256, config.prompt ? sha256Buffer(Buffer.from(config.prompt, 'utf8')) : undefined),
			rules: compactObject({
				general: firstDefined(scenarioConfig.rules?.general, metadata.general_rules, config.general_rules),
				task_specific: firstDefined(scenarioConfig.rules?.task_specific, metadata.task_rules, config.task_rules),
			}),
		}),
		task_set: compactObject({
			id: firstDefined(taskSetConfig.id, metadataEval.task_set_id, configEval.task_set_id),
			version: firstDefined(taskSetConfig.version, metadataEval.task_set_version, configEval.task_set_version),
			benchmark_status: firstDefined(taskSetConfig.benchmark_status, metadataEval.benchmark_status, configEval.benchmark_status),
			compatibility_group: firstDefined(taskSetConfig.compatibility_group, metadataEval.compatibility_group, configEval.compatibility_group),
			aggregate_score: firstDefined(taskSetConfig.aggregate_score, metadataEval.aggregate_score, configEval.aggregate_score),
			headline_score_eligible: firstDefined(taskSetConfig.headline_score_eligible, metadataEval.headline_score_eligible, configEval.headline_score_eligible),
		}),
		grader: compactObject({
			success,
			reward,
			grade,
			failure_reasons: Array.isArray(failureReasons) ? failureReasons : [],
			checks: firstDefined(graderConfig.checks, metadata.grader_checks, metadata.checks, []),
		}),
		status,
	});
}

function buildBundle(results, scenario, config, bundlePath, resultsPath, episodeJsonlPath, episodeRows) {
	const metadata = scenario.metadata && typeof scenario.metadata === 'object' ? scenario.metadata : {};
	const reviewUrl = resolveReviewUrl(config, scenario);
	const references = artifactReferences(scenario, config, bundlePath, episodeJsonlPath);
	const artifactIntegrity = hashArtifactReferences(references, resultsPath);

	return redact({
		schema_version: 1,
		generated_at: new Date().toISOString(),
		sealed_eval_artifact: buildSealedEnvelope(results, scenario, config, bundlePath, artifactIntegrity, episodeJsonlPath, episodeRows, {
			projections: ({ scenario: projectionScenario, config: projectionConfig, metadata: projectionMetadata, evalArtifact }) => ({
				wp_gym: buildWpGymProjection(projectionScenario, projectionConfig, projectionMetadata, evalArtifact),
			}),
		}),
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
		initial_blueprint: config.wordpress_runtime_blueprint || {},
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
			reason: 'The current WP Codebox bench command exits after scenario execution and does not export a restorable final site state.',
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

function attachBundle(results, scenarioId, relativeBundlePath, episodeJsonlPath, reviewUrl) {
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
					episode_jsonl: {
						path: episodeJsonlPath,
						kind: 'jsonl',
						label: 'Episode JSONL',
					},
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
	const episodeFilename = `${safeId(args.scenario)}-episode.jsonl`;
	const bundlePath = path.join(args.outputDir, filename);
	const episodePath = path.join(args.outputDir, episodeFilename);
	const relativeBundlePath = path.relative(path.dirname(args.results), bundlePath) || filename;
	const relativeEpisodePath = path.relative(path.dirname(args.results), episodePath) || episodeFilename;
	const episodeRows = writeEpisodeJsonl(episodePath, scenario, config);
	const bundle = buildBundle(results, scenario, config, relativeBundlePath, args.results, relativeEpisodePath, episodeRows);
	if (bundle.sealed_eval_artifact && bundle.sealed_eval_artifact.hashes) {
		bundle.sealed_eval_artifact.hashes.envelope = sha256Json(bundle.sealed_eval_artifact);
	}
	fs.writeFileSync(bundlePath, `${JSON.stringify(bundle, null, 2)}\n`);

	if (args.updateResults) {
		const updated = attachBundle(results, args.scenario, relativeBundlePath, relativeEpisodePath, resolveReviewUrl(config, scenario));
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
