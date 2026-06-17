'use strict';

const BROWSER_RESULT_SCHEMA_VERSION = 1;
const HOMEBOY_BENCH_RESULTS_SCHEMA = 'homeboy/bench-results/v1';
const HOMEBOY_BROWSER_EVIDENCE_SCHEMA = 'homeboy/browser-evidence/v1';

const VALID_TRACE_ASSERTION_STATUSES = new Set(['pass', 'fail', 'skip', 'unknown']);
const VALID_TRACE_ENVELOPE_STATUSES = new Set(['pass', 'fail', 'error', 'skip', 'unknown']);

function normalizeBrowserArtifact(artifact, defaults = {}) {
	const source = artifact && typeof artifact === 'object' && !Array.isArray(artifact) ? artifact : {};
	const normalized = {
		path: stringValue(source.path ?? source.relativePath ?? defaults.path),
	};
	const kind = stringValue(source.kind ?? defaults.kind);
	const label = stringValue(source.label ?? defaults.label);
	if (kind) normalized.kind = kind;
	if (label) normalized.label = label;
	return stableJson(normalized);
}

function normalizeBrowserNetworkRequest(entry) {
	const source = entry && typeof entry === 'object' && !Array.isArray(entry) ? entry : {};
	return stableJson({
		url: stringValue(source.url ?? source.name),
		method: stringValue(source.method ?? source.request_method ?? source.requestMethod).toUpperCase(),
		resource_type: stringValue(source.resource_type ?? source.resourceType ?? source.initiator_type ?? source.initiatorType),
		status: finiteOrNull(source.status ?? source.statusCode ?? source.status_code ?? source.http_status),
		failed: Boolean(source.failed),
		start_time_ms: finiteOrNull(source.start_time_ms ?? source.startTime ?? source.start_ms ?? source.startMs),
		duration_ms: finiteOrNull(source.duration_ms ?? source.durationMs ?? source.duration),
		failure_text: stringValue(source.failure_text ?? source.failureText) || undefined,
	});
}

function normalizeBrowserPhaseMark(mark) {
	const source = mark && typeof mark === 'object' && !Array.isArray(mark) ? mark : {};
	return stableJson({
		name: sanitizePhaseName(source.name ?? source.phase ?? source.label),
		start_time_ms: finiteNumber(source.start_time_ms ?? source.startTime ?? source.start_ms ?? source.startMs),
	});
}

function collectBrowserPhases(phaseMarks) {
	const marks = Array.isArray(phaseMarks)
		? phaseMarks.map(normalizeBrowserPhaseMark).filter((mark) => mark.name).sort(comparePhaseMarks)
		: [];
	const phases = {};
	for (let index = 0; index < marks.length; index += 1) {
		const current = marks[index];
		const next = marks[index + 1];
		phases[current.name] = {
			start_time_ms: current.start_time_ms,
			end_time_ms: next ? next.start_time_ms : null,
			duration_ms: next ? Math.max(0, roundNumber(next.start_time_ms - current.start_time_ms)) : 0,
		};
	}
	return stableJson(phases);
}

function normalizeBrowserBottleneck(row) {
	const source = row && typeof row === 'object' && !Array.isArray(row) ? row : {};
	const normalized = {
		kind: stringValue(source.kind) || 'unknown',
		phase: stringValue(source.phase) || 'all',
		message: stringValue(source.message),
	};
	if (source.data !== undefined) normalized.data = source.data;
	return stableJson(normalized);
}

function normalizeBrowserPerformanceProfile(profile) {
	const source = profile && typeof profile === 'object' && !Array.isArray(profile) ? profile : {};
	const phaseMarks = Array.isArray(source.phase_marks) ? source.phase_marks.map(normalizeBrowserPhaseMark).filter((mark) => mark.name).sort(comparePhaseMarks) : [];
	const phases = normalizePhaseMap(source.phases, phaseMarks);
	return stableJson({
		schema_version: finiteNumber(source.schema_version) || BROWSER_RESULT_SCHEMA_VERSION,
		page_url: stringValue(source.page_url ?? source.url),
		summary: source.summary && typeof source.summary === 'object' && !Array.isArray(source.summary) ? stableJson(source.summary) : {},
		navigation: Array.isArray(source.navigation) ? source.navigation.map(stableJson) : [],
		resources: Array.isArray(source.resources) ? source.resources.map(stableJson) : [],
		network: Array.isArray(source.network) ? source.network.map(stableJson) : [],
		console_messages: Array.isArray(source.console_messages) ? source.console_messages.map(stableJson) : [],
		page_errors: Array.isArray(source.page_errors) ? source.page_errors.map(stableJson) : [],
		paints: Array.isArray(source.paints) ? source.paints.map(stableJson) : [],
		largest_contentful_paint: Array.isArray(source.largest_contentful_paint) ? source.largest_contentful_paint.map(stableJson) : [],
		layout_shifts: Array.isArray(source.layout_shifts) ? source.layout_shifts.map(stableJson) : [],
		long_tasks: Array.isArray(source.long_tasks) ? source.long_tasks.map(stableJson) : [],
		phase_marks: phaseMarks,
		phases,
	});
}

function normalizeBrowserTiming(entry, options = {}) {
	if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
		return null;
	}

	const rawUrl = pickFirstString(entry, ['name', 'url', 'request_url', 'requestUrl']);
	if (!rawUrl) {
		return null;
	}

	const normalizeUrl = typeof options.normalizeUrl === 'function' ? options.normalizeUrl : defaultNormalizeUrl;
	const startTime = pickFirstNumber(entry, ['startTime', 'fetchStart', 'requestStart', 'start_time_ms', 'start_ms', 'startMs']);
	const responseStart = pickFirstNumber(entry, ['responseStart', 'ttfb_ms', 'ttfbMs', 'response_start']);
	const responseEnd = pickFirstNumber(entry, ['responseEnd', 'response_end', 'endMs', 'end_ms', 'response_end_ms']);
	const duration = pickFirstNumber(entry, ['duration', 'duration_ms', 'durationMs']);

	let computedDuration = duration;
	if (computedDuration === undefined && startTime !== undefined && responseEnd !== undefined) {
		computedDuration = Math.max(0, responseEnd - startTime);
	}

	let computedTtfb;
	if (responseStart !== undefined && startTime !== undefined) {
		computedTtfb = Math.max(0, responseStart - startTime);
	} else {
		computedTtfb = pickFirstNumber(entry, ['ttfb', 'ttfb_ms', 'ttfbMs']);
	}

	const initiator = pickFirstString(entry, ['initiatorType', 'initiator_type', 'initiator', 'resourceType']);
	const phase = pickFirstString(entry, ['phase', 'phase_label', 'phaseLabel']);
	const method = pickFirstString(entry, ['method', 'request_method', 'requestMethod']);
	const status = pickFirstNumber(entry, ['status', 'statusCode', 'status_code', 'http_status']);
	const failed = pickFirstBoolean(entry, ['failed', 'error']);

	return {
		url: rawUrl,
		normalizedUrl: normalizeUrl(rawUrl, options),
		method: method ? method.toUpperCase() : undefined,
		status: typeof status === 'number' ? status : undefined,
		failed,
		startTime: typeof startTime === 'number' ? startTime : undefined,
		ttfbMs: typeof computedTtfb === 'number' ? computedTtfb : undefined,
		durationMs: typeof computedDuration === 'number' ? computedDuration : undefined,
		initiatorType: initiator,
		phase,
		raw: entry,
	};
}

function normalizeBrowserProfileTimings(profile, options = {}) {
	if (!profile || typeof profile !== 'object' || Array.isArray(profile)) {
		return [];
	}

	const normalizeUrl = typeof options.normalizeUrl === 'function' ? options.normalizeUrl : defaultNormalizeUrl;
	const phases = normalizeProfilePhases(profile);
	const resources = Array.isArray(profile.resources) ? profile.resources : [];
	const network = Array.isArray(profile.network) ? profile.network : [];
	const resourcesByUrl = new Map();

	for (const resource of resources) {
		const rawUrl = pickFirstString(resource, ['name', 'url']);
		const normalizedUrl = normalizeUrl(rawUrl, options);
		if (!normalizedUrl) continue;
		if (!resourcesByUrl.has(normalizedUrl)) resourcesByUrl.set(normalizedUrl, []);
		resourcesByUrl.get(normalizedUrl).push(resource);
	}

	const rows = [];
	for (const entry of network) {
		const rawUrl = pickFirstString(entry, ['url', 'name']);
		const normalizedUrl = normalizeUrl(rawUrl, options);
		const resource = normalizedUrl ? (resourcesByUrl.get(normalizedUrl) || []).shift() : undefined;
		const startTime = pickFirstNumber(entry, ['start_time_ms', 'startTime', 'start_ms', 'startMs'])
			?? pickFirstNumber(resource, ['startTime', 'fetchStart', 'requestStart']);
		const phase = pickFirstString(entry, ['phase', 'phase_label', 'phaseLabel'])
			|| pickFirstString(resource, ['phase', 'phase_label', 'phaseLabel'])
			|| phaseForStartTime(phases, startTime);

		rows.push({
			...resource,
			...entry,
			name: rawUrl,
			url: rawUrl,
			startTime,
			responseStart: pickFirstNumber(resource, ['responseStart']) ?? pickFirstNumber(entry, ['responseStart', 'ttfb_ms', 'ttfbMs']),
			responseEnd: pickFirstNumber(resource, ['responseEnd']) ?? pickFirstNumber(entry, ['response_end_ms', 'responseEnd']),
			durationMs: pickFirstNumber(entry, ['duration_ms', 'durationMs']) ?? pickFirstNumber(resource, ['duration', 'duration_ms', 'durationMs']),
			phase,
		});
	}

	for (const entries of resourcesByUrl.values()) {
		for (const resource of entries) {
			const startTime = pickFirstNumber(resource, ['startTime', 'fetchStart', 'requestStart']);
			rows.push({
				...resource,
				phase: pickFirstString(resource, ['phase', 'phase_label', 'phaseLabel']) || phaseForStartTime(phases, startTime),
			});
		}
	}

	return rows;
}

function normalizeTraceEvent(source, event, data = {}, timestampMs = undefined) {
	const entry = {
		t_ms: finiteNumber(timestampMs),
		source: stringValue(source) || 'scenario',
		event: stringValue(event),
		data: data && typeof data === 'object' && !Array.isArray(data) ? stableJson(data) : { value: data },
	};
	return stableJson(entry);
}

function normalizeTraceAssertion(id, status, message, data = undefined) {
	const assertion = {
		id: stringValue(id),
		status: VALID_TRACE_ASSERTION_STATUSES.has(status) ? status : 'unknown',
		message: stringValue(message),
	};
	if (data !== undefined) assertion.data = data;
	return stableJson(assertion);
}

function normalizeTraceEnvelope(envelope) {
	const source = envelope && typeof envelope === 'object' && !Array.isArray(envelope) ? envelope : {};
	const normalized = {
		component_id: stringValue(source.component_id ?? source.componentId) || 'unknown',
		scenario_id: stringValue(source.scenario_id ?? source.scenarioId) || 'unknown',
		status: VALID_TRACE_ENVELOPE_STATUSES.has(source.status) ? source.status : 'unknown',
		summary: stringValue(source.summary),
		timeline: Array.isArray(source.timeline) ? source.timeline.map((entry) => stableJson(entry)) : [],
		assertions: Array.isArray(source.assertions) ? source.assertions.map((assertion) => stableJson(assertion)) : [],
		artifacts: Array.isArray(source.artifacts) ? source.artifacts.map(normalizeBrowserArtifact) : [],
	};
	if (source.metrics !== undefined) normalized.metrics = stableJson(source.metrics);
	if (source.failure !== undefined) normalized.failure = source.failure;
	return stableJson(normalized);
}

function buildBenchResultsEnvelope({ componentId, iterations = 1, scenarios = [] } = {}) {
	const normalizedComponentId = stringValue(componentId);
	if (!normalizedComponentId) throw new Error('buildBenchResultsEnvelope requires componentId.');
	return stableJson({
		schema: HOMEBOY_BENCH_RESULTS_SCHEMA,
		component_id: normalizedComponentId,
		iterations: positiveInteger(iterations, 1),
		scenarios: Array.isArray(scenarios) ? scenarios.map(buildBenchScenarioResult) : [],
	});
}

function buildBenchScenarioResult(input = {}) {
	const source = input && typeof input === 'object' && !Array.isArray(input) ? input : {};
	const id = stringValue(source.id ?? source.scenario_id ?? source.scenarioId);
	if (!id) throw new Error('buildBenchScenarioResult requires id.');

	const scenario = {
		id,
		iterations: positiveInteger(source.iterations, 1),
		metrics: normalizeBenchMetrics(source.metrics),
	};
	copyOptionalString(scenario, 'file', source.file);
	copyOptionalString(scenario, 'source', source.source);
	copyOptionalInteger(scenario, 'default_iterations', source.default_iterations ?? source.defaultIterations);
	copyOptionalArray(scenario, 'tags', source.tags);
	copyOptionalObject(scenario, 'metric_groups', source.metric_groups ?? source.metricGroups);
	copyOptionalArray(scenario, 'timeline', source.timeline);
	copyOptionalArray(scenario, 'span_definitions', source.span_definitions ?? source.spanDefinitions);
	copyOptionalArray(scenario, 'span_results', source.span_results ?? source.spanResults);
	copyOptionalArray(scenario, 'gates', source.gates);
	copyOptionalArray(scenario, 'gate_results', source.gate_results ?? source.gateResults);
	copyOptionalObject(scenario, 'metadata', source.metadata);
	copyOptionalObject(scenario, 'provenance', source.provenance);
	if (source.passed !== undefined) scenario.passed = Boolean(source.passed);
	copyOptionalObject(scenario, 'memory', source.memory);
	const artifacts = normalizeBenchArtifacts(source.artifacts);
	if (Object.keys(artifacts).length > 0) scenario.artifacts = artifacts;
	copyOptionalArray(scenario, 'diagnostics', source.diagnostics);
	copyOptionalArray(scenario, 'runs', source.runs);
	copyOptionalObject(scenario, 'runs_summary', source.runs_summary ?? source.runsSummary);
	return stableJson(scenario);
}

function buildBrowserBenchResult(input = {}) {
	const source = input && typeof input === 'object' && !Array.isArray(input) ? input : {};
	const browserResult = source.browserResult && typeof source.browserResult === 'object' && !Array.isArray(source.browserResult) ? source.browserResult : {};
	const metrics = normalizeBenchMetrics({
		...(browserResult.metrics || {}),
		...(source.browserMetrics || {}),
		...(source.metrics || {}),
	});
	const artifacts = normalizeBenchArtifacts({
		...(browserResult.artifacts || {}),
		...(source.browserArtifacts || {}),
		...(source.rawResultArtifact ? { raw_result: source.rawResultArtifact } : {}),
		...(source.artifacts || {}),
	});
	const metadata = stableJson({
		browser_evidence_schema: HOMEBOY_BROWSER_EVIDENCE_SCHEMA,
		...(source.metadata || {}),
	});

	return stableJson({
		metrics,
		...(Object.keys(artifacts).length > 0 ? { artifacts } : {}),
		...(Object.keys(metadata).length > 0 ? { metadata } : {}),
	});
}

function normalizeBenchMetrics(metrics = {}) {
	if (!metrics || typeof metrics !== 'object' || Array.isArray(metrics)) return {};
	const normalized = {};
	for (const [key, value] of Object.entries(metrics).sort(([a], [b]) => a.localeCompare(b))) {
		if (typeof value === 'number' && Number.isFinite(value)) normalized[key] = roundNumber(value);
	}
	return normalized;
}

function normalizeBenchArtifacts(artifacts = {}) {
	if (!artifacts || typeof artifacts !== 'object' || Array.isArray(artifacts)) return {};
	const normalized = {};
	for (const [key, artifact] of Object.entries(artifacts).sort(([a], [b]) => a.localeCompare(b))) {
		const descriptor = normalizeBenchArtifact(artifact);
		if (Object.keys(descriptor).length > 0) normalized[key] = descriptor;
	}
	return normalized;
}

function normalizeBenchArtifact(artifact) {
	if (typeof artifact === 'string' && artifact.trim()) return { path: artifact.trim() };
	if (!artifact || typeof artifact !== 'object' || Array.isArray(artifact)) return {};
	const normalized = {};
	for (const key of ['path', 'url', 'type', 'kind', 'label', 'observation_artifact_id', 'role', 'preview_url', 'public_url', 'viewer_url', 'local_url', 'status']) {
		copyOptionalString(normalized, key, artifact[key]);
	}
	for (const key of ['viewer', 'browser_origin_evidence', 'service_lifecycle']) {
		copyOptionalObject(normalized, key, artifact[key]);
	}
	copyOptionalArray(normalized, 'viewer_links', artifact.viewer_links ?? artifact.viewerLinks);
	copyOptionalString(normalized, 'expires_at', artifact.expires_at ?? artifact.expiresAt);
	copyOptionalString(normalized, 'cleanup_status', artifact.cleanup_status ?? artifact.cleanupStatus);
	return stableJson(normalized);
}

function copyOptionalString(target, key, value) {
	if (typeof value === 'string' && value.trim() !== '') target[key] = value.trim();
}

function copyOptionalInteger(target, key, value) {
	const number = Number(value);
	if (Number.isInteger(number) && number >= 0) target[key] = number;
}

function copyOptionalArray(target, key, value) {
	if (Array.isArray(value) && value.length > 0) target[key] = value.map(stableJson);
}

function copyOptionalObject(target, key, value) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) return;
	const normalized = stableJson(value);
	if (Object.keys(normalized).length > 0) target[key] = normalized;
}

function positiveInteger(value, fallback) {
	const number = Number(value);
	return Number.isInteger(number) && number > 0 ? number : fallback;
}

function normalizePhaseMap(phases, phaseMarks) {
	if (!phases || typeof phases !== 'object' || Array.isArray(phases)) {
		return collectBrowserPhases(phaseMarks);
	}
	const normalized = {};
	for (const [name, phase] of Object.entries(phases).sort(([a], [b]) => a.localeCompare(b))) {
		if (!phase || typeof phase !== 'object' || Array.isArray(phase)) continue;
		normalized[sanitizePhaseName(name)] = {
			start_time_ms: finiteNumber(phase.start_time_ms ?? phase.startTime ?? phase.start_ms ?? phase.startMs),
			end_time_ms: finiteOrNull(phase.end_time_ms ?? phase.endTime ?? phase.end_ms ?? phase.endMs),
			duration_ms: finiteNumber(phase.duration_ms ?? phase.durationMs ?? phase.duration),
		};
	}
	return stableJson(normalized);
}

function normalizeProfilePhases(profile) {
	const phases = [];
	if (profile.phases && typeof profile.phases === 'object' && !Array.isArray(profile.phases)) {
		for (const [name, phase] of Object.entries(profile.phases)) {
			if (!phase || typeof phase !== 'object') continue;
			const startMs = pickFirstNumber(phase, ['start_time_ms', 'startTime', 'start_ms', 'startMs']);
			const endMs = pickFirstNumber(phase, ['end_time_ms', 'endTime', 'end_ms', 'endMs']);
			if (startMs !== undefined) phases.push({ name, startMs, endMs });
		}
	} else if (Array.isArray(profile.phase_marks)) {
		const marks = profile.phase_marks
			.map((mark) => ({
				name: pickFirstString(mark, ['name', 'phase', 'label']),
				startMs: pickFirstNumber(mark, ['start_time_ms', 'startTime', 'start_ms', 'startMs']),
			}))
			.filter((mark) => mark.name && mark.startMs !== undefined)
			.sort((a, b) => a.startMs - b.startMs);
		for (let index = 0; index < marks.length; index += 1) {
			phases.push({
				name: marks[index].name,
				startMs: marks[index].startMs,
				endMs: marks[index + 1]?.startMs,
			});
		}
	}
	return phases.sort((a, b) => a.startMs - b.startMs);
}

function phaseForStartTime(phases, startTime) {
	if (!Array.isArray(phases) || typeof startTime !== 'number') return undefined;
	let matched;
	for (const phase of phases) {
		const endMs = typeof phase.endMs === 'number' ? phase.endMs : Infinity;
		if (startTime >= phase.startMs && startTime < endMs) matched = phase.name;
	}
	return matched;
}

function defaultNormalizeUrl(url) {
	if (typeof url !== 'string' || url.trim() === '') return '';
	try {
		const parsed = new URL(url.trim(), 'http://__homeboy_stub__');
		return parsed.pathname + parsed.search;
	} catch {
		return url.trim();
	}
}

function pickFirstNumber(source, keys) {
	if (!source || typeof source !== 'object') return undefined;
	for (const key of keys) {
		const value = source[key];
		if (typeof value === 'number' && Number.isFinite(value)) return value;
	}
	return undefined;
}

function pickFirstString(source, keys) {
	if (!source || typeof source !== 'object') return undefined;
	for (const key of keys) {
		const value = source[key];
		if (typeof value === 'string' && value.trim() !== '') return value.trim();
	}
	return undefined;
}

function pickFirstBoolean(source, keys) {
	if (!source || typeof source !== 'object') return undefined;
	for (const key of keys) {
		const value = source[key];
		if (typeof value === 'boolean') return value;
	}
	return undefined;
}

function finiteNumber(value) {
	return typeof value === 'number' && Number.isFinite(value) ? roundNumber(value) : 0;
}

function finiteOrNull(value) {
	return typeof value === 'number' && Number.isFinite(value) ? roundNumber(value) : null;
}

function roundNumber(value) {
	return typeof value === 'number' && Number.isFinite(value) ? Math.round(value * 1000) / 1000 : value;
}

function stringValue(value) {
	return typeof value === 'string' ? value.trim() : '';
}

function sanitizePhaseName(name) {
	return String(name || '').trim().toLowerCase().replace(/[^a-z0-9_.:-]+/g, '_').replace(/^_+|_+$/g, '') || 'phase';
}

function comparePhaseMarks(a, b) {
	return finiteNumber(a.start_time_ms) - finiteNumber(b.start_time_ms) || String(a.name).localeCompare(String(b.name));
}

function stableJson(value) {
	if (Array.isArray(value)) return value.map(stableJson);
	if (!value || typeof value !== 'object') return value;
	return Object.fromEntries(
		Object.entries(value)
			.filter(([, item]) => item !== undefined)
			.sort(([a], [b]) => a.localeCompare(b))
			.map(([key, item]) => [key, stableJson(item)])
	);
}

module.exports = {
	BROWSER_RESULT_SCHEMA_VERSION,
	HOMEBOY_BENCH_RESULTS_SCHEMA,
	HOMEBOY_BROWSER_EVIDENCE_SCHEMA,
	buildBenchResultsEnvelope,
	buildBenchScenarioResult,
	buildBrowserBenchResult,
	collectBrowserPhases,
	normalizeBrowserArtifact,
	normalizeBrowserBottleneck,
	normalizeBrowserNetworkRequest,
	normalizeBrowserPerformanceProfile,
	normalizeBrowserPhaseMark,
	normalizeBrowserProfileTimings,
	normalizeBrowserTiming,
	normalizeTraceAssertion,
	normalizeTraceEnvelope,
	normalizeTraceEvent,
	stableJson,
};
