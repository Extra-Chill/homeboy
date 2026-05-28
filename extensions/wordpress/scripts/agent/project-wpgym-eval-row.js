#!/usr/bin/env node
/* eslint-disable no-console */
'use strict';

/**
 * External dependencies
 */
const fs = require('fs');
const crypto = require('crypto');

const CONTRACT_ISSUE = 'https://github.com/Automattic/wp-gym/issues/117';
const PROJECTION_ISSUE = 'https://github.com/Extra-Chill/homeboy-extensions/issues/807';
const SCHEMA_NAME = 'wp-gym.eval_artifact_row';
const SCHEMA_VERSION = 0;
const PROJECTION_VERSION = 'homeboy-extensions.compat.1';

function usage() {
	console.error('Usage: project-wpgym-eval-row.js --results <path> --scenario <id> --config <path> [--update-results] [--benchmark-mode]');
}

function parseArgs(argv) {
	const args = {};
	for (let index = 0; index < argv.length; index += 1) {
		const arg = argv[index];
		if (arg === '--update-results') {
			args.updateResults = true;
			continue;
		}
		if (arg === '--benchmark-mode') {
			args.benchmarkMode = true;
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

function stableValue(value) {
	if (Array.isArray(value)) {
		return value.map((entry) => stableValue(entry));
	}
	if (value && typeof value === 'object') {
		return Object.fromEntries(Object.keys(value).sort().map((key) => [key, stableValue(value[key])]));
	}
	return value;
}

function sha256Json(value) {
	return `sha256:${crypto.createHash('sha256').update(JSON.stringify(stableValue(value))).digest('hex')}`;
}

function compactObject(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => {
		if (entry === undefined || entry === null || entry === '') {
			return false;
		}
		if (Array.isArray(entry) && entry.length === 0) {
			return false;
		}
		if (entry && typeof entry === 'object' && !Array.isArray(entry) && Object.keys(entry).length === 0) {
			return false;
		}
		return true;
	}));
}

function present(value) {
	if (value === undefined || value === null || value === '') {
		return false;
	}
	if (Array.isArray(value)) {
		return value.length > 0;
	}
	if (typeof value === 'object') {
		if (Object.prototype.hasOwnProperty.call(value, 'available')) {
			return value.available === true && present(value.path ?? value.value ?? value.url ?? value.href ?? value.sha256 ?? value.status);
		}
		return Object.keys(value).length > 0;
	}
	return true;
}

function referenceValue(reference) {
	if (!reference || typeof reference !== 'object') {
		return '';
	}
	return reference.path ?? reference.value ?? reference.url ?? reference.href ?? '';
}

function evidenceReference(scenario, name) {
	const references = scenario.metadata?.evidence_references?.references;
	if (!references || typeof references !== 'object') {
		return null;
	}
	return references[name] || null;
}

function artifactReference(scenario, name) {
	const artifact = scenario.artifacts?.[name];
	if (!artifact || typeof artifact !== 'object') {
		return null;
	}
	return {
		kind: artifact.kind || 'artifact',
		path: artifact.path || '',
		label: artifact.label || name,
		source: 'homeboy',
		available: present(artifact.path),
	};
}

function firstReference(scenario, ...names) {
	for (const name of names) {
		const reference = evidenceReference(scenario, name) || artifactReference(scenario, name);
		if (present(reference)) {
			return reference;
		}
		if (reference) {
			return reference;
		}
	}
	return null;
}

function requiredReferenceGaps(references, requirePullRequest) {
	const required = [
		['replay_bundle', references.replay_bundle, 'Replay bundle artifact is required for wp-gym replay rows.'],
		['episode_trace', references.episode_trace, 'Runtime episode trace is required for action/replay projection.'],
		['transcript', references.transcript, 'Transcript artifact is required for model-observation provenance.'],
		['verifier', references.verifier, 'Verifier result is required for benchmark-mode grading.'],
		['policy', references.policy, 'Policy attestation is required to separate allowed orchestration from eval semantics.'],
		['workflow', references.workflow, 'Workflow run reference is required for artifact provenance.'],
		['homeboy_result', references.homeboy_result, 'Homeboy result JSON is required as the source projection artifact.'],
	];

	if (requirePullRequest) {
		required.push(['pull_request', references.pull_request, 'Pull request reference is required when success_requires_pr is enabled.']);
	}

	return required
		.filter(([, reference]) => !present(reference))
		.map(([field, reference, reason]) => ({
			field,
			reason,
			compatibility_gap: true,
			available: reference?.available === true,
		}));
}

function projectionReferences(scenario) {
	return {
		replay_bundle: firstReference(scenario, 'replay_bundle_artifact', 'replay_bundle'),
		episode_trace: firstReference(scenario, 'runtime_episode_trace', 'episode_jsonl'),
		transcript: firstReference(scenario, 'transcript_artifact', 'transcript_json'),
		verifier: firstReference(scenario, 'artifact_verifier_result'),
		policy: firstReference(scenario, 'workspace_policy_result'),
		workflow: firstReference(scenario, 'workflow_run'),
		pull_request: firstReference(scenario, 'pull_request'),
		homeboy_result: firstReference(scenario, 'homeboy_result_json'),
		artifact_bundle: firstReference(scenario, 'wp_codebox_artifact_bundle'),
	};
}

function projectionStatus(gaps, benchmarkMode) {
	if (gaps.length === 0) {
		return 'ready';
	}
	return benchmarkMode ? 'invalid' : 'compatibility_gaps';
}

function projectEvalRow(results, scenario, config, benchmarkMode) {
	const metadata = scenario.metadata && typeof scenario.metadata === 'object' ? scenario.metadata : {};
	const evalArtifact = metadata.eval_artifact && typeof metadata.eval_artifact === 'object' ? metadata.eval_artifact : {};
	const sealed = metadata.replay_bundle?.sealed_eval_artifact || evalArtifact.envelope || {};
	const references = projectionReferences(scenario);
	const requirePullRequest = config.success_requires_pr === true;
	const gaps = [
		...requiredReferenceGaps(references, requirePullRequest),
		...(Array.isArray(metadata.evidence_references?.compatibility_gaps) ? metadata.evidence_references.compatibility_gaps : []),
		...(Array.isArray(sealed.integration_seams) ? sealed.integration_seams.map((field) => ({ field, reason: 'Homeboy sealed artifact reports this integration seam as missing.', compatibility_gap: true })) : []),
	];
	const normalizedGaps = Array.from(new Map(gaps.map((gap) => [gap.field, gap])).values());
	const outcome = compactObject({
		status: metadata.job_status || evalArtifact.run?.job_status || '',
		success_status: metadata.success_status || evalArtifact.run?.success_status || '',
		completion_outcome: metadata.completion_outcome || '',
		completion_outcome_satisfied: metadata.completion_outcome_satisfied === true,
		reward: metadata.reward ?? metadata.score ?? evalArtifact.grade?.reward ?? evalArtifact.grade?.score,
		failure_reasons: evalArtifact.failure_reasons || metadata.failure_reasons || [],
		termination: evalArtifact.termination || {},
	});
	const evaluation = compactObject({
		task: compactObject({
			id: metadata.task_id || config.task_id || config.workload_id || scenario.id,
			label: metadata.task_label || config.task_label || config.workload_label || scenario.label,
		}),
		subject: compactObject({
			target_repo: metadata.target_repo || config.target_repo,
			bundle_repo: metadata.bundle_repo || config.bundle_repo,
			bundle_ref: metadata.bundle_ref || config.bundle_ref,
			bundle_path: metadata.bundle_path || config.bundle_path,
		}),
		agent: compactObject({
			slug: metadata.agent_slug || config.agent_slug,
			id: metadata.agent_id,
		}),
		model: compactObject({
			provider: metadata.provider || config.provider,
			model: metadata.model || config.model,
		}),
		prompt: evalArtifact.prompt || evalArtifact.hashes?.prompt || metadata.fingerprints?.prompt || (config.prompt ? {
			sha256: `sha256:${crypto.createHash('sha256').update(config.prompt).digest('hex')}`,
			bytes: Buffer.byteLength(config.prompt),
		} : {}),
		outcome,
		grade: evalArtifact.grade || metadata.grade || {},
		rules: compactObject({
			general: metadata.general_rules || config.general_rules || [],
			task_specific: metadata.task_rules || config.task_rules || [],
			results: metadata.general_rule_results || [],
			policy: metadata.rules || config.rules || {},
		}),
		probes: metadata.probes || config.probes || {},
		replay: compactObject({
			format: sealed.replay?.format || 'jsonl',
			episode_row_count: sealed.replay?.episode_row_count,
			tool_audit_event_count: sealed.replay?.tool_audit_event_count ?? evalArtifact.replay?.tool_audit_event_count,
			references: compactObject({
				replay_bundle: referenceValue(references.replay_bundle),
				episode_trace: referenceValue(references.episode_trace),
				transcript: referenceValue(references.transcript),
			}),
		}),
	});
	const orchestration = compactObject({
		homeboy: compactObject({
			component_id: results.component_id || results.component,
			scenario_id: scenario.id,
			result_json: referenceValue(references.homeboy_result),
			artifact_bundle: referenceValue(references.artifact_bundle),
		}),
		datamachine: compactObject({
			job_id: metadata.job_id || evalArtifact.run?.job_id,
			pipeline_id: metadata.pipeline_id,
			flow_id: metadata.flow_id,
			transcript_session_id: metadata.transcript_session_id,
		}),
		data_machine_code: compactObject({
			workspace: metadata.runner_workspace || {},
			workspace_capture: metadata.runner_workspace_capture || {},
			policy_attestation: referenceValue(references.policy) || metadata.datamachine_code_policy_attestation,
		}),
		github: compactObject({
			workflow_run: referenceValue(references.workflow),
			pull_request: referenceValue(references.pull_request),
		}),
		artifacts: compactObject({
			verifier: referenceValue(references.verifier),
			policy: referenceValue(references.policy),
			workflow: referenceValue(references.workflow),
			all_references: references,
		}),
	});
	const row = {
		schema_name: SCHEMA_NAME,
		schema_version: SCHEMA_VERSION,
		projection_version: PROJECTION_VERSION,
		contract_status: 'compatibility_scaffold',
		contract_issue: CONTRACT_ISSUE,
		projection_issue: PROJECTION_ISSUE,
		projected_at: new Date().toISOString(),
		status: projectionStatus(normalizedGaps, benchmarkMode),
		benchmark_mode: benchmarkMode,
		evaluation,
		orchestration,
		compatibility_gaps: normalizedGaps,
	};
	row.row_sha256 = sha256Json(row);
	return row;
}

function attachProjection(results, scenarioId, row) {
	return {
		...results,
		scenarios: (results.scenarios || []).map((scenario) => {
			if (scenario.id !== scenarioId) {
				return scenario;
			}
			return {
				...scenario,
				metadata: {
					...(scenario.metadata || {}),
					wp_gym_eval_row: row,
				},
			};
		}),
	};
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
	const benchmarkMode = args.benchmarkMode || process.env.HOMEBOY_WPGYM_BENCHMARK_MODE === '1' || config.wp_gym_eval?.benchmark_mode === true;
	const scenario = (results.scenarios || []).find((entry) => entry.id === args.scenario);
	if (!scenario) {
		throw new Error(`Scenario not found in results: ${args.scenario}`);
	}

	const row = projectEvalRow(results, scenario, config, benchmarkMode);
	if (benchmarkMode && row.compatibility_gaps.length > 0) {
		console.error(`ERROR: wp-gym eval row projection has ${row.compatibility_gaps.length} benchmark-mode compatibility gap(s).`);
		console.error(JSON.stringify(row.compatibility_gaps, null, 2));
		process.exitCode = 1;
	}

	if (args.updateResults) {
		const updated = attachProjection(results, args.scenario, row);
		fs.writeFileSync(args.results, `${JSON.stringify(updated, null, 2)}\n`);
	}

	console.log(JSON.stringify(row, null, 2));
}

try {
	main();
} catch (error) {
	console.error(`ERROR: ${error.message}`);
	process.exit(1);
}
