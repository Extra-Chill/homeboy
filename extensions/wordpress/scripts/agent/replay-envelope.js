'use strict';

/**
 * External dependencies
 */
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const SECRET_KEY_PATTERN = /(secret|token|password|passwd|authorization|cookie|nonce|api[_-]?key|access[_-]?key|private[_-]?key|github[_-]?token|openai[_-]?api[_-]?key)/i;

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

function stableValue(value) {
	if (Array.isArray(value)) {
		return value.map((entry) => stableValue(entry));
	}
	if (value && typeof value === 'object') {
		return Object.fromEntries(Object.keys(value).sort().map((key) => [key, stableValue(value[key])]));
	}
	return value;
}

function sha256Buffer(buffer) {
	return `sha256:${crypto.createHash('sha256').update(buffer).digest('hex')}`;
}

function sha256Json(value) {
	return sha256Buffer(Buffer.from(JSON.stringify(stableValue(value)), 'utf8'));
}

function fileSha256(filePath) {
	return sha256Buffer(fs.readFileSync(filePath));
}

function isRemoteReference(referencePath) {
	return /^https?:\/\//i.test(String(referencePath || ''));
}

function resolveLocalArtifactPath(referencePath, resultsPath) {
	if (!referencePath || path.isAbsolute(referencePath) || isRemoteReference(referencePath)) {
		return referencePath || '';
	}
	return path.resolve(path.dirname(resultsPath), referencePath);
}

function artifactReferences(scenario, config, bundlePath, episodeJsonlPath = '') {
	const artifacts = scenario.artifacts && typeof scenario.artifacts === 'object' ? scenario.artifacts : {};
	const references = [];

	for (const [name, artifact] of Object.entries(artifacts)) {
		if (artifact && typeof artifact === 'object' && artifact.path) {
			references.push({ name, path: artifact.path, kind: artifact.kind || 'artifact', required: /transcript|episode|replay|grade/i.test(name) });
		}
	}

	const metadata = scenario.metadata && typeof scenario.metadata === 'object' ? scenario.metadata : {};
	const transcriptArtifacts = metadata.transcript_artifacts && typeof metadata.transcript_artifacts === 'object' ? metadata.transcript_artifacts : {};
	for (const [name, referencePath] of Object.entries({
		transcript_json: metadata.transcript_json,
		transcript_summary: metadata.transcript_summary,
		action_log: metadata.action_log,
		transcript_artifact_json: transcriptArtifacts.json,
		transcript_artifact_summary: transcriptArtifacts.summary,
	})) {
		if (referencePath) {
			references.push({ name, path: referencePath, kind: 'metadata', required: /transcript|action/.test(name) });
		}
	}

	if (config.transcript_dir) {
		references.push({ name: 'transcript_dir', path: config.transcript_dir, kind: 'directory', required: false });
	}
	if (bundlePath) {
		references.push({ name: 'replay_bundle', path: bundlePath, kind: 'json', required: false });
	}
	if (episodeJsonlPath) {
		references.push({ name: 'episode_jsonl', path: episodeJsonlPath, kind: 'jsonl', required: true });
	}

	return references;
}

function hashArtifactReferences(references, resultsPath) {
	const hashes = {};
	const issues = [];
	for (const reference of references) {
		if (!reference.path) {
			continue;
		}
		const normalizedPath = String(reference.path);
		if (isRemoteReference(normalizedPath)) {
			if (reference.required) {
				issues.push({ type: 'remote_required_artifact', name: reference.name, path: normalizedPath });
			}
			continue;
		}

		const localPath = resolveLocalArtifactPath(normalizedPath, resultsPath);
		if (!fs.existsSync(localPath) || !fs.statSync(localPath).isFile()) {
			if (reference.required) {
				issues.push({ type: 'missing_required_artifact', name: reference.name, path: normalizedPath });
			}
			continue;
		}

		hashes[reference.name] = {
			path: normalizedPath,
			sha256: fileSha256(localPath),
			bytes: fs.statSync(localPath).size,
		};
	}

	return { hashes, issues };
}

function normalizeToolAuditEvents(metadata) {
	const evalArtifact = metadata.eval_artifact && typeof metadata.eval_artifact === 'object' ? metadata.eval_artifact : {};
	const replay = evalArtifact.replay && typeof evalArtifact.replay === 'object' ? evalArtifact.replay : {};
	const candidates = [metadata.tool_audit_events, replay.tool_audit_events];
	const events = candidates.find((entry) => Array.isArray(entry)) || [];

	return events.filter((event) => event && typeof event === 'object').map((event, index) => compactObject({
		schema_version: event.schema_version || 1,
		type: event.type || 'tool_call',
		step_index: index,
		actor: event.actor || 'agent',
		observation_channels: Array.isArray(event.observation_channels) ? event.observation_channels : undefined,
		turn_count: event.turn_count,
		tool_name: event.tool_name || event.action_name,
		tool_source: event.tool_source,
		parameters_sha256: event.parameters_sha256 || event.args_sha256,
		parameters_redacted: event.parameters_redacted !== false,
		success: event.success === true,
		result_status: event.result_status || (event.success === true ? 'success' : 'error'),
		result_sha256: event.result_sha256,
		error_type: event.error_type,
	}));
}

function replaySharedMetadata(scenario, config, metadata, evalArtifact) {
	const fingerprints = metadata.fingerprints && typeof metadata.fingerprints === 'object' ? metadata.fingerprints : {};
	const evalHashes = evalArtifact.hashes && typeof evalArtifact.hashes === 'object' ? evalArtifact.hashes : {};
	const toolPolicy = {
		required_abilities: config.required_abilities || [],
		ability_tools: config.ability_tools || [],
		tool_recorders: config.tool_recorders || [],
		pipeline_step_patches: config.pipeline_step_patches || [],
		flow_step_patches: config.flow_step_patches || [],
		runner_workspace: config.runner_workspace || {},
		success_requires_pr: config.success_requires_pr === true,
		success_completion_outcomes: config.success_completion_outcomes || [],
	};

	return compactObject({
		scenario_id: scenario.id,
		task_id: metadata.task_id || config.task_id || config.workload_id || scenario.id,
		job_id: metadata.job_id || (evalArtifact.run && evalArtifact.run.job_id),
		reset_hash: config.reset_hash || metadata.reset_hash,
		prompt_sha256: (fingerprints.prompt && fingerprints.prompt.sha256) || (evalHashes.prompt && evalHashes.prompt.sha256) || (config.prompt ? sha256Buffer(Buffer.from(config.prompt, 'utf8')) : undefined),
		bundle_sha256: (fingerprints.bundle && fingerprints.bundle.sha256) || (evalHashes.bundle && evalHashes.bundle.sha256),
		tool_policy_sha256: (fingerprints.tool_policy && fingerprints.tool_policy.sha256) || (evalHashes.tool_policy && evalHashes.tool_policy.sha256) || sha256Json(toolPolicy),
	});
}

function replayObservationChannels(metadata, config) {
	const channels = metadata.observation_channels || config.observation_channels || [];
	return Array.isArray(channels) ? channels.filter((channel) => typeof channel === 'string' && channel.length > 0) : [];
}

function buildEpisodeRows(scenario, config) {
	const metadata = scenario.metadata && typeof scenario.metadata === 'object' ? scenario.metadata : {};
	const evalArtifact = metadata.eval_artifact && typeof metadata.eval_artifact === 'object' ? metadata.eval_artifact : {};
	const toolAuditEvents = normalizeToolAuditEvents(metadata);
	const shared = replaySharedMetadata(scenario, config, metadata, evalArtifact);
	const observationChannels = replayObservationChannels(metadata, config);
	const rows = [];

	for (const event of toolAuditEvents) {
		rows.push(redact(compactObject({
			schema_name: 'homeboy.agent_episode_step',
			schema_version: 1,
			step_index: rows.length,
			row_type: 'action',
			actor: event.actor || 'agent',
			observation_channels: event.observation_channels || observationChannels,
			action_name: event.tool_name,
			tool_name: event.tool_name,
			tool_source: event.tool_source,
			args_sha256: event.parameters_sha256,
			args_redacted: event.parameters_redacted !== false,
			result_status: event.result_status || (event.success === true ? 'success' : 'error'),
			result_sha256: event.result_sha256,
			error_type: event.error_type,
			shared,
		})));
	}

	const grade = evalArtifact.grade || metadata.grade || {};
	const failureReasons = evalArtifact.failure_reasons || metadata.failure_reasons || [];
	const reward = metadata.reward ?? metadata.score ?? (grade.reward ?? grade.score);
	rows.push(redact(compactObject({
		schema_name: 'homeboy.agent_episode_step',
		schema_version: 1,
		step_index: rows.length,
		row_type: 'grader',
		actor: 'grader',
		terminal: true,
		observation_channels: ['grader'],
		action_name: 'terminal_grader',
		result_status: metadata.job_status || (evalArtifact.run && evalArtifact.run.job_status) || 'unknown',
		reward,
		failure_reasons: Array.isArray(failureReasons) ? failureReasons : [],
		grade,
		shared,
	})));

	return rows;
}

function writeEpisodeJsonl(filePath, scenario, config) {
	const rows = buildEpisodeRows(scenario, config);
	fs.writeFileSync(filePath, `${rows.map((row) => JSON.stringify(row)).join('\n')}\n`);
	return rows;
}

function buildSealedEnvelope(results, scenario, config, bundlePath, artifactIntegrity, episodeJsonlPath, episodeRows, adapters = {}) {
	const metadata = scenario.metadata && typeof scenario.metadata === 'object' ? scenario.metadata : {};
	const evalArtifact = metadata.eval_artifact && typeof metadata.eval_artifact === 'object' ? metadata.eval_artifact : {};
	const fingerprints = metadata.fingerprints && typeof metadata.fingerprints === 'object' ? metadata.fingerprints : {};
	const toolAuditEvents = normalizeToolAuditEvents(metadata);
	const missingSeams = [];
	const attestation = evalArtifact.attestation && typeof evalArtifact.attestation === 'object' ? evalArtifact.attestation : {};
	const existingSeams = Array.isArray(attestation.integration_seams) ? attestation.integration_seams : [];
	missingSeams.push(...existingSeams);
	if (!attestation.runtime_provenance || Object.keys(attestation.runtime_provenance).length === 0) {
		missingSeams.push('runtime_provenance');
	}
	if (!attestation.workspace_policy_attestation || Object.keys(attestation.workspace_policy_attestation).length === 0) {
		missingSeams.push('workspace_policy_attestation');
	}
	if (toolAuditEvents.length === 0) {
		missingSeams.push('agents_api_tool_audit_events');
	}
	const env = adapters.env || process.env;
	const workflowRunUrl = metadata.workflow_run_url || (evalArtifact.run && evalArtifact.run.workflow_run_url) || (
		env.GITHUB_REPOSITORY && env.GITHUB_RUN_ID
			? `${env.GITHUB_SERVER_URL || 'https://github.com'}/${env.GITHUB_REPOSITORY}/actions/runs/${env.GITHUB_RUN_ID}`
			: ''
	);
	const projections = typeof adapters.projections === 'function'
		? adapters.projections({ results, scenario, config, metadata, evalArtifact })
		: {};

	return redact({
		schema_name: 'homeboy.sealed_eval_artifact',
		schema_version: 1,
		generated_at: new Date().toISOString(),
		status: artifactIntegrity.issues.length === 0 && toolAuditEvents.length > 0 ? 'ready_for_replay' : 'incomplete',
		runner: compactObject({
			ref: env.GITHUB_SHA || config.runner_ref || metadata.runner_ref,
			workflow_run_url: workflowRunUrl,
			job_id: env.GITHUB_JOB || config.github_job,
		}),
		run: compactObject({
			job_id: metadata.job_id || (evalArtifact.run && evalArtifact.run.job_id),
			job_status: metadata.job_status || (evalArtifact.run && evalArtifact.run.job_status),
			success_status: metadata.success_status || (evalArtifact.run && evalArtifact.run.success_status),
		}),
		task: compactObject({
			id: metadata.task_id || config.task_id || config.workload_id || scenario.id,
			label: metadata.task_label || config.task_label || config.workload_label || scenario.label,
		}),
		model: compactObject({
			provider: config.provider || metadata.provider,
			model: config.model || metadata.model,
		}),
		hashes: compactObject({
			prompt: fingerprints.prompt || (evalArtifact.hashes && evalArtifact.hashes.prompt) || (config.prompt ? { sha256: sha256Buffer(Buffer.from(config.prompt, 'utf8')), bytes: Buffer.byteLength(config.prompt) } : undefined),
			bundle: fingerprints.bundle || (evalArtifact.hashes && evalArtifact.hashes.bundle),
			tool_policy: fingerprints.tool_policy || (evalArtifact.hashes && evalArtifact.hashes.tool_policy),
			reset: config.reset_hash || metadata.reset_hash || undefined,
			artifact_hashes: artifactIntegrity.hashes,
		}),
		grade: evalArtifact.grade || metadata.grade || {},
		failure_reasons: evalArtifact.failure_reasons || metadata.failure_reasons || [],
		termination: evalArtifact.termination || compactObject({ state: metadata.job_status, truncated: metadata.truncated === true }),
		replay: {
			format: 'jsonl',
			episode_jsonl: episodeJsonlPath,
			episode_row_count: episodeRows.length,
			tool_audit_events_source: 'Agents API result.tool_audit_events',
			tool_audit_event_count: toolAuditEvents.length,
			tool_audit_events: toolAuditEvents,
		},
		artifacts: {
			references: artifactReferences(scenario, config, bundlePath, episodeJsonlPath),
			hashes: artifactIntegrity.hashes,
			issues: artifactIntegrity.issues,
		},
		...projections,
		integration_seams: Array.from(new Set(missingSeams.filter(Boolean))),
	});
}

module.exports = {
	SECRET_KEY_PATTERN,
	safeId,
	redact,
	compactObject,
	sha256Buffer,
	sha256Json,
	isRemoteReference,
	resolveLocalArtifactPath,
	artifactReferences,
	hashArtifactReferences,
	normalizeToolAuditEvents,
	replaySharedMetadata,
	replayObservationChannels,
	buildEpisodeRows,
	writeEpisodeJsonl,
	buildSealedEnvelope,
};
