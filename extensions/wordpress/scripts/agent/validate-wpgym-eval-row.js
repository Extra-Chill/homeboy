#!/usr/bin/env node
/* eslint-disable no-console */
'use strict';

/**
 * External dependencies
 */
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const SCHEMA_NAME = 'wp-gym.eval_artifact_row';
const SHA256_PATTERN = /^sha256:[a-f0-9]{64}$/;

function usage() {
	console.error('Usage: validate-wpgym-eval-row.js --results <path> --scenario <id> --config <path>');
}

function parseArgs(argv) {
	const args = {};
	for (let index = 0; index < argv.length; index += 1) {
		const arg = argv[index];
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

function sha256File(filePath) {
	return `sha256:${crypto.createHash('sha256').update(fs.readFileSync(filePath)).digest('hex')}`;
}

function isRemoteReference(value) {
	return /^https?:\/\//i.test(String(value || ''));
}

function referencePath(reference) {
	if (!reference || typeof reference !== 'object') {
		return '';
	}
	return reference.path ?? reference.value ?? reference.url ?? reference.href ?? '';
}

function resolveReferencePath(referenceValue, resultsPath) {
	if (!referenceValue || path.isAbsolute(referenceValue) || isRemoteReference(referenceValue)) {
		return referenceValue || '';
	}
	return path.resolve(path.dirname(resultsPath), referenceValue);
}

function hasObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length > 0;
}

function validateJsonl(filePath, issueField, issues) {
	const text = fs.readFileSync(filePath, 'utf8');
	const rows = [];
	text.split(/\r?\n/).forEach((line, index) => {
		if (line.trim() === '') {
			return;
		}
		try {
			rows.push(JSON.parse(line));
		} catch (error) {
			issues.push({ field: issueField, reason: `Invalid JSONL at line ${index + 1}: ${error.message}` });
		}
	});
	if (rows.length === 0) {
		issues.push({ field: issueField, reason: 'Episode JSONL must contain at least one row.' });
	}
	return rows;
}

function findHash(hashSources, names, actualSha256) {
	for (const source of hashSources) {
		if (!source || typeof source !== 'object') {
			continue;
		}
		for (const name of names) {
			const hashValue = source[name]?.sha256 || source[name]?.hash || source[name];
			if (typeof hashValue === 'string' && hashValue === actualSha256) {
				return hashValue;
			}
		}
	}
	return '';
}

function expectedArtifactNames(scenario, config) {
	const metadata = hasObject(scenario.metadata) ? scenario.metadata : {};
	const manifest = hasObject(metadata.scenario_manifest) ? metadata.scenario_manifest : {};
	const evalConfig = hasObject(config.wp_gym_eval) ? config.wp_gym_eval : {};
	const candidates = [
		config.expected_artifacts,
		evalConfig.expected_artifacts,
		metadata.expected_artifacts,
		manifest.expected_artifacts,
	];
	return candidates.find((entry) => Array.isArray(entry)) || [];
}

function valuesEqual(left, right) {
	if (left === undefined || right === undefined) {
		return true;
	}
	return JSON.stringify(left) === JSON.stringify(right);
}

function validateTerminalGrade(row, episodeRows, issues) {
	const terminal = [...episodeRows].reverse().find((entry) => entry && typeof entry === 'object' && (entry.terminal === true || entry.row_type === 'grader'));
	if (!terminal) {
		issues.push({ field: 'episode_trace', reason: 'Episode JSONL must include a terminal grader row.' });
		return;
	}

	const rowGrade = row.evaluation?.grade || {};
	const rowReward = row.evaluation?.outcome?.reward;
	if (!valuesEqual(terminal.grade?.score, rowGrade.score) || !valuesEqual(terminal.grade?.max_score, rowGrade.max_score) || !valuesEqual(terminal.reward, rowReward)) {
		issues.push({ field: 'terminal_grade', reason: 'Terminal episode grader row does not match the projected eval row grade/reward.' });
	}
}

function validateLocalArtifact(name, reference, resultsPath, hashSources, aliases, issues, options = {}) {
	const rawPath = referencePath(reference);
	if (!rawPath) {
		issues.push({ field: name, reason: `${name} reference is missing.` });
		return { localPath: '', rows: [] };
	}
	if (isRemoteReference(rawPath)) {
		issues.push({ field: name, reason: `${name} must be a local artifact, not a remote URL.` });
		return { localPath: '', rows: [] };
	}

	const localPath = resolveReferencePath(rawPath, resultsPath);
	if (!fs.existsSync(localPath) || !fs.statSync(localPath).isFile()) {
		issues.push({ field: name, reason: `${name} local artifact does not exist: ${rawPath}` });
		return { localPath, rows: [] };
	}

	const actualSha256 = sha256File(localPath);
	const referenceHash = reference.sha256 || reference.hash || '';
	if (referenceHash && !SHA256_PATTERN.test(referenceHash)) {
		issues.push({ field: name, reason: `${name} hash is not a sha256:<hex> digest.` });
	} else if (referenceHash && referenceHash !== actualSha256) {
		issues.push({ field: name, reason: `${name} hash does not match the local artifact.` });
	} else if (options.requireHash !== false && !referenceHash && !findHash(hashSources, [name, ...aliases], actualSha256)) {
		issues.push({ field: name, reason: `${name} local artifact is missing a matching sha256 hash.` });
	}

	return { localPath, rows: [] };
}

function validateEvalRow(resultsPath, scenario, config) {
	const issues = [];
	const row = scenario.metadata?.wp_gym_eval_row;
	if (!hasObject(row)) {
		return [{ field: 'wp_gym_eval_row', reason: 'Scenario is missing metadata.wp_gym_eval_row.' }];
	}
	if (row.schema_name !== SCHEMA_NAME) {
		issues.push({ field: 'schema_name', reason: `Expected schema_name ${SCHEMA_NAME}.` });
	}
	if (row.status !== 'ready') {
		issues.push({ field: 'status', reason: 'Projected eval row must be ready before it can be trusted as benchmark evidence.' });
	}
	if (Array.isArray(row.compatibility_gaps) && row.compatibility_gaps.length > 0) {
		issues.push({ field: 'compatibility_gaps', reason: 'Projected eval row still has compatibility gaps.' });
	}

	const references = row.orchestration?.artifacts?.all_references || {};
	const replayReference = references.replay_bundle;
	const replayPath = referencePath(replayReference);
	let replayBundle = {};
	if (replayPath && !isRemoteReference(replayPath)) {
		const localReplayPath = resolveReferencePath(replayPath, resultsPath);
		if (fs.existsSync(localReplayPath) && fs.statSync(localReplayPath).isFile()) {
			replayBundle = readJson(localReplayPath);
		}
	}

	const sealed = replayBundle.sealed_eval_artifact || scenario.metadata?.replay_bundle?.sealed_eval_artifact || {};
	const artifactHashes = sealed.hashes?.artifact_hashes || {};
	const hashSources = [artifactHashes, sealed.hashes || {}, replayBundle.hashes || {}];
	const requiredArtifacts = [
		['homeboy_result', references.homeboy_result, ['homeboy_result_json'], { requireHash: false }],
		['transcript', references.transcript, ['transcript_json', 'transcript_artifact', 'transcript_artifact_json'], {}],
		['episode_trace', references.episode_trace, ['episode_jsonl', 'runtime_episode_trace'], {}],
		['replay_bundle', replayReference, ['replay_bundle_artifact'], { requireHash: false }],
	];

	const localArtifacts = new Map();
	for (const [name, reference, aliases, options] of requiredArtifacts) {
		localArtifacts.set(name, validateLocalArtifact(name, reference, resultsPath, hashSources, aliases, issues, options));
	}

	if (!SHA256_PATTERN.test(sealed.hashes?.envelope || '')) {
		issues.push({ field: 'replay_bundle', reason: 'Replay bundle must include sealed_eval_artifact.hashes.envelope.' });
	}

	let episodeRows = [];
	const episode = localArtifacts.get('episode_trace');
	if (episode?.localPath && fs.existsSync(episode.localPath)) {
		episodeRows = validateJsonl(episode.localPath, 'episode_trace', issues);
	}
	validateTerminalGrade(row, episodeRows, issues);

	for (const [field, reference] of [
		['verifier', references.verifier],
		['policy', references.policy],
		['grader_result', references.grader_result],
	]) {
		if (!hasObject(reference) || reference.available !== true) {
			issues.push({ field, reason: `${field} evidence reference is missing or unavailable.` });
		}
	}

	for (const expected of expectedArtifactNames(scenario, config)) {
		const name = typeof expected === 'string' ? expected : expected?.name;
		if (!name) {
			continue;
		}
		const artifact = scenario.artifacts?.[name] || references[name];
		validateLocalArtifact(`expected_artifacts.${name}`, artifact, resultsPath, hashSources, [name], issues);
	}

	return issues;
}

function main() {
	const args = parseArgs(process.argv.slice(2));
	for (const required of ['results', 'scenario', 'config']) {
		if (!args[required]) {
			usage();
			throw new Error(`Missing --${required}`);
		}
	}

	const results = readJson(args.results);
	const config = readJson(args.config);
	const scenario = (results.scenarios || []).find((entry) => entry.id === args.scenario);
	if (!scenario) {
		throw new Error(`Scenario not found in results: ${args.scenario}`);
	}

	const issues = validateEvalRow(args.results, scenario, config);
	if (issues.length > 0) {
		console.error(`ERROR: wp-gym live-run eval row validation failed with ${issues.length} issue(s).`);
		console.error(JSON.stringify(issues, null, 2));
		process.exit(1);
	}

	console.log(JSON.stringify({ valid: true, scenario: args.scenario, schema_name: SCHEMA_NAME }, null, 2));
}

try {
	main();
} catch (error) {
	console.error(`ERROR: ${error.message}`);
	process.exit(1);
}
