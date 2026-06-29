'use strict';

const WORDPRESS_FUZZ_RUNTIME_TASK_REQUEST_SCHEMA = 'homeboy/fuzz-runtime-task/v1';
const WORDPRESS_FUZZ_RUNTIME_TASK_RESULT_SCHEMA = 'homeboy/fuzz-runtime-task-result/v1';
const WORDPRESS_FUZZ_HOTSPOT_SET_SCHEMA = 'homeboy/fuzz-hotspot-set/v1';
const WORDPRESS_FUZZ_HOTSPOT_SUMMARY_SCHEMA = WORDPRESS_FUZZ_HOTSPOT_SET_SCHEMA;
const WORDPRESS_FUZZ_OBSERVATION_SET_SCHEMA = 'homeboy/fuzz-observation-set/v1';
const WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA = 'wp-codebox/wordpress-hotspots/v1';
const WP_CODEBOX_WORDPRESS_OBSERVATIONS_SCHEMA = 'wp-codebox/wordpress-observations/v1';

function buildWordPressFuzzRuntimeTaskRequest(options = {}) {
	const provider = normalizeProvider(options.provider || options.runtimeId || options.runtime_id || 'wp-codebox');
	return stripUndefined({
		schema: WORDPRESS_FUZZ_RUNTIME_TASK_REQUEST_SCHEMA,
		task_id: options.taskId || options.task_id || options.id,
		workload_id: options.workloadId || options.workload_id,
		provider,
		input: objectOrUndefined(options.input),
		requirements: objectOrUndefined(options.requirements || options.runtimeRequirements || options.runtime_requirements),
		artifact_declarations: normalizeArray(options.artifactDeclarations || options.artifact_declarations),
		expected_artifacts: normalizeArray(options.expectedArtifacts || options.expected_artifacts),
		instructions: nonEmptyString(options.instructions || options.goal),
		provider_request: objectOrUndefined(options.providerRequest || options.provider_request),
		provider_metadata: objectOrUndefined(options.providerMetadata || options.provider_metadata),
		metadata: objectOrUndefined(options.metadata),
	});
}

function normalizeWordPressFuzzRuntimeTaskResult(result = {}, context = {}) {
	const source = objectOrUndefined(result) || {};
	const provider = normalizeProvider(context.provider || source.provider || source.runtime || source.provider_id || source.providerId || 'wp-codebox');
	const status = normalizeStatus(source.status || source.outcome?.status || context.status);
	const succeeded = source.succeeded === undefined ? succeededFromStatus(status) : source.succeeded === true;
	const hotspotSummary = normalizeFuzzHotspotSummary(
		source.hotspot_summary || source.hotspotSummary || source.hotspots || source.hotspot_report || source.hotspotReport || source.performance_hotspots || source.performanceHotspots,
		{ provider: provider.id, taskId: source.request_id || source.requestId || context.taskId || context.task_id }
	);
	const observationSet = normalizeFuzzObservationSet(
		source.observation_set || source.observationSet || source.observations || source.measurements || source.performance || source.query_data || source.queryData,
		{ provider: provider.id, taskId: source.request_id || source.requestId || context.taskId || context.task_id }
	);

	return stripUndefined({
		schema: WORDPRESS_FUZZ_RUNTIME_TASK_RESULT_SCHEMA,
		task_id: source.request_id || source.requestId || context.taskId || context.task_id,
		provider,
		status,
		succeeded,
		coverage: source.coverage,
		coverage_summary: objectOrUndefined(source.coverage_summary || source.coverageSummary),
		coverage_gaps: source.coverage_gaps || source.coverageGaps,
		observation_set: observationSet,
		hotspot_summary: hotspotSummary,
		artifacts: normalizeArray(source.artifacts || source.artifact_refs || source.artifactRefs),
		failures: normalizeArray(source.failures || source.errors || source.diagnostics),
		provider_result: objectOrUndefined(source.provider_result || source.providerResult),
		provider_metadata: objectOrUndefined(source.provider_metadata || source.providerMetadata),
		metadata: objectOrUndefined(source.metadata),
	});
}

function normalizeFuzzObservationSet(input, defaults = {}) {
	const source = objectOrUndefined(input) ? input : { observations: input };
	const observations = codeboxMeasurementCandidates(source)
		.map((entry, index) => normalizeFuzzObservation(entry, { ...defaults, index }))
		.filter(Boolean);
	if (observations.length === 0) {
		return undefined;
	}
	return stripUndefined({
		schema: WORDPRESS_FUZZ_OBSERVATION_SET_SCHEMA,
		version: 1,
		id: nonEmptyString(source.id || source.observation_set_id || source.observationSetId) || `${defaults.taskId || defaults.provider || 'wordpress'}-observations`,
		label: nonEmptyString(source.label),
		observations,
		metadata: objectOrUndefined(source.metadata),
	});
}

function codeboxMeasurementCandidates(source = {}) {
	const candidates = [
		source.observations,
		source.items,
		source.measurements,
		source.metrics,
		source.performance_measurements,
		source.performanceMeasurements,
		...codeboxWordPressObservationCandidates(source),
	];
	const plurals = { query: 'queries', action: 'actions', resource: 'resources', timing: 'timings', counter: 'counters', cache: 'caches', write: 'writes' };
	for (const family of ['query', 'action', 'resource', 'timing', 'counter', 'cache', 'write']) {
		candidates.push(source[family], source[plurals[family]], source[`${family}_measurements`], source[`${family}Measurements`]);
	}
	return candidates.flatMap(normalizeArray);
}

function codeboxWordPressObservationCandidates(source = {}) {
	const performanceDatabase = source.performance?.database || source.database;
	return [
		...normalizeArray(source.sql || source.sql_observations || source.sqlObservations).map((entry) => ({ family: 'query', ...entry })),
		...normalizeArray(source.cache_observations || source.cacheObservations).map((entry) => ({ family: 'cache', ...entry })),
		...normalizeArray(source.write_observations || source.writeObservations || source.database_writes || source.databaseWrites).map((entry) => ({ family: 'write', ...entry })),
		...normalizeArray(source.action_correlations || source.actionCorrelations).map((entry) => ({ family: 'action', metric: 'correlation_count', ...entry })),
		...normalizeArray(performanceDatabase?.fingerprints).map((entry) => ({ family: 'query', category: 'query_fingerprint', ...entry })),
		...normalizeArray(performanceDatabase?.repeatedQueries || performanceDatabase?.repeated_queries).map((entry) => ({ family: 'query', category: 'duplicate_query_group', duplicate: true, ...entry })),
	];
}

function normalizeFuzzObservation(entry = {}, defaults = {}) {
	if (!objectOrUndefined(entry)) {
		return undefined;
	}
	const metadata = objectOrUndefined(entry.metadata) || {};
	const family = normalizeObservationFamily(entry.family || entry.type || entry.kind || metadata.family || inferObservationFamily(entry));
	const metric = nonEmptyString(entry.metric || entry.metric_name || entry.metricName || entry.name || inferMetric(entry));
	const value = numericValue(entry.value ?? entry.metric_value ?? entry.metricValue ?? entry.count ?? entry.total ?? entry.duration_ms ?? entry.durationMs ?? entry.elapsed_ms ?? entry.elapsedMs ?? entry.totalTimeMs ?? entry.total_time_ms ?? entry.sampleMs ?? entry.sample_ms ?? entry.memory_bytes ?? entry.memoryBytes ?? entry.bytes ?? entry.rows ?? entry.sets ?? entry.deletes ?? entry.hits ?? entry.misses);
	const subject = nonEmptyString(entry.subject || entry.query || entry.sql || entry.fingerprint || entry.table || entry.cache_key || entry.cacheKey || entry.transient || entry.option || entry.action || entry.hook || entry.resource || entry.endpoint || entry.route || entry.url || entry.name || metric);
	if (!family || !metric || value === undefined || !subject) {
		return undefined;
	}
	const category = normalizeObservationCategory(entry.category || entry.group || entry.grouping || metadata.category || inferObservationCategory(entry, family));
	const caseId = nonEmptyString(entry.case_id || entry.caseId || metadata.case_id || metadata.caseId);
	const targetId = nonEmptyString(entry.target_id || entry.targetId || entry.surface_id || entry.surfaceId || metadata.target_id || metadata.targetId || metadata.surface_id || metadata.surfaceId);
	const operationId = nonEmptyString(entry.operation_id || entry.operationId || entry.operation || metadata.operation_id || metadata.operationId || metadata.operation);
	const action = nonEmptyString(entry.action || entry.hook || metadata.action || metadata.hook);
	const workloadId = nonEmptyString(entry.workload_id || entry.workloadId || metadata.workload_id || metadata.workloadId);
	return stripUndefined({
		id: nonEmptyString(entry.id || entry.key) || observationId({ defaults, family, caseId, targetId, operationId, metric, subject }),
		family,
		case_id: caseId,
		target_id: targetId,
		operation_id: operationId,
		phase: nonEmptyString(entry.phase || metadata.phase),
		subject,
		metric,
		value,
		unit: nonEmptyString(entry.unit || inferObservationUnit(entry, metric, family)) || 'count',
		fingerprint: nonEmptyString(entry.fingerprint || entry.hash || entry.signature || metadata.fingerprint),
		sample_count: positiveInteger(entry.sample_count || entry.sampleCount || entry.samples, undefined),
		metadata: stripUndefined({
			...metadata,
			provider: defaults.provider,
			category,
			group_key: observationGroupKey(entry, category, subject),
			table: nonEmptyString(entry.table || metadata.table),
			cache_key: nonEmptyString(entry.cache_key || entry.cacheKey || entry.key || metadata.cache_key || metadata.cacheKey),
			cache_group: nonEmptyString(entry.cache_group || entry.cacheGroup || entry.group || metadata.cache_group || metadata.cacheGroup),
			option: nonEmptyString(entry.option || entry.option_name || entry.optionName || metadata.option || metadata.option_name || metadata.optionName),
			autoload: booleanOrUndefined(entry.autoload ?? metadata.autoload),
			write_family: nonEmptyString(entry.write_family || entry.writeFamily || entry.operation || entry.verb || metadata.write_family || metadata.writeFamily),
			action,
			workload_id: workloadId,
		}),
	});
}

function observationId({ defaults = {}, family, caseId, targetId, operationId, metric, subject }) {
	return [defaults.taskId || defaults.provider || 'wordpress', family, caseId, targetId, operationId, metric, subject]
		.filter(Boolean)
		.join(':')
		.replace(/\s+/g, '-');
}

function normalizeObservationFamily(value) {
	const family = String(value || '').trim().toLowerCase().replace(/[\s.-]+/g, '_');
	if (['action', 'query', 'resource', 'timing', 'counter', 'rollback', 'cache', 'write'].includes(family)) {
		return family;
	}
	return undefined;
}

function normalizeObservationCategory(value) {
	const category = String(value || '').trim().toLowerCase().replace(/[\s.-]+/g, '_');
	if (['query_fingerprint', 'table', 'duplicate_query_group', 'cache_key_group', 'transient_key_group', 'option_autoload_churn', 'db_write_family', 'action_case_workload_correlation'].includes(category)) {
		return category;
	}
	return undefined;
}

function inferObservationCategory(entry = {}, family) {
	if (entry.duplicate === true || entry.repeated === true || entry.duplicate_group || entry.duplicateGroup) {
		return 'duplicate_query_group';
	}
	if (family === 'query' && (entry.fingerprint || entry.query || entry.sql)) {
		return 'query_fingerprint';
	}
	if (family === 'query' && entry.table) {
		return 'table';
	}
	if (family === 'cache' && (entry.transient === true || /^_transient_/.test(String(entry.key || entry.cache_key || entry.cacheKey || '')))) {
		return 'transient_key_group';
	}
	if (family === 'cache') {
		return 'cache_key_group';
	}
	if (family === 'write' && (entry.option || entry.option_name || entry.optionName || entry.autoload !== undefined)) {
		return 'option_autoload_churn';
	}
	if (family === 'write') {
		return 'db_write_family';
	}
	if (family === 'action') {
		return 'action_case_workload_correlation';
	}
	return undefined;
}

function observationGroupKey(entry = {}, category, subject) {
	if (category === 'query_fingerprint' || category === 'duplicate_query_group') {
		return nonEmptyString(entry.fingerprint || entry.query || entry.sql || subject);
	}
	if (category === 'table') {
		return nonEmptyString(entry.table || subject);
	}
	if (category === 'cache_key_group' || category === 'transient_key_group') {
		return [entry.cache_group || entry.cacheGroup || entry.group, entry.cache_key || entry.cacheKey || entry.key || entry.transient || subject].filter(Boolean).join(':');
	}
	if (category === 'option_autoload_churn') {
		return [entry.option || entry.option_name || entry.optionName || subject, entry.autoload === undefined ? undefined : `autoload:${entry.autoload === true ? 'yes' : 'no'}`].filter(Boolean).join(':');
	}
	if (category === 'db_write_family') {
		return [entry.table, entry.write_family || entry.writeFamily || entry.operation || entry.verb || subject].filter(Boolean).join(':');
	}
	if (category === 'action_case_workload_correlation') {
		return [entry.action || entry.hook, entry.case_id || entry.caseId, entry.workload_id || entry.workloadId].filter(Boolean).join(':') || subject;
	}
	return subject;
}

function inferObservationFamily(entry = {}) {
	if (entry.query !== undefined || entry.sql !== undefined || entry.query_count !== undefined || entry.queryCount !== undefined) {
		return 'query';
	}
	if (entry.cache_key !== undefined || entry.cacheKey !== undefined || entry.cache_group !== undefined || entry.cacheGroup !== undefined || entry.transient !== undefined) {
		return 'cache';
	}
	if (entry.write_family !== undefined || entry.writeFamily !== undefined || entry.table !== undefined || entry.rows !== undefined || entry.autoload !== undefined || entry.option !== undefined || entry.option_name !== undefined || entry.optionName !== undefined) {
		return 'write';
	}
	if (entry.action !== undefined || entry.hook !== undefined) {
		return 'action';
	}
	if (entry.duration_ms !== undefined || entry.durationMs !== undefined || entry.elapsed_ms !== undefined || entry.elapsedMs !== undefined) {
		return 'timing';
	}
	if (entry.memory_bytes !== undefined || entry.memoryBytes !== undefined || entry.memory_peak_bytes !== undefined || entry.memoryPeakBytes !== undefined || entry.bytes !== undefined) {
		return 'resource';
	}
	if (entry.count !== undefined || entry.total !== undefined) {
		return 'counter';
	}
	return undefined;
}

function inferObservationUnit(entry = {}, metric = '', family = '') {
	if (/duration|elapsed|time|latency/.test(metric) || entry.duration_ms !== undefined || entry.durationMs !== undefined || entry.elapsed_ms !== undefined || entry.elapsedMs !== undefined) {
		return 'ms';
	}
	if (/bytes|memory/.test(metric) || family === 'resource') {
		return 'bytes';
	}
	return 'count';
}

function fuzzHotspotSummaryFromObservationSet(input, defaults = {}) {
	const observationSet = input?.schema === WORDPRESS_FUZZ_OBSERVATION_SET_SCHEMA ? input : normalizeFuzzObservationSet(input, defaults);
	if (!observationSet) {
		return undefined;
	}
	return normalizeFuzzHotspotSummary({
		items: groupedHotspotItemsFromObservations(observationSet.observations),
		metadata: { observation_set_id: observationSet.id },
	}, defaults);
}

function groupedHotspotItemsFromObservations(observations = []) {
	const groups = new Map();
	for (const observation of observations) {
		const metadata = observation.metadata || {};
		const category = metadata.category || observation.family;
		const groupKey = metadata.group_key || observation.fingerprint || observation.subject;
		const operationKey = [observation.operation_id, observation.case_id, metadata.action, metadata.workload_id].filter(Boolean).join(':') || observation.phase || observation.subject;
		const key = [category, groupKey, operationKey, observation.metric].join('\u0000');
		const current = groups.get(key) || {
			surface_key: groupKey,
			operation_key: operationKey,
			dimension: category,
			metric: observation.metric,
			value: 0,
			unit: observation.unit,
			sample_count: 0,
			metadata: stripUndefined({ category, family: observation.family, group_key: groupKey, observations: [] }),
		};
		current.value = Number((current.value + observation.value).toFixed(6));
		current.sample_count += observation.sample_count || 1;
		current.metadata.observations.push(observation.id);
		groups.set(key, current);
	}
	const items = Array.from(groups.values()).sort((a, b) => b.value - a.value || String(a.surface_key).localeCompare(String(b.surface_key)));
	const max = items.reduce((largest, item) => Math.max(largest, item.value), 0) || 1;
	return items.map((item, index) => ({
		...item,
		rank: index + 1,
		relative_score: Number((item.value / max).toFixed(6)),
	}));
}

function normalizeFuzzHotspotSummary(input, defaults = {}) {
	const source = objectOrUndefined(input) ? input : { items: input };
	const rawItems = codeboxWordPressHotspotItems(source);
	const summaryDefaults = {
		...defaults,
		dimension: nonEmptyString(source.dimension || source.kind || defaults.dimension),
		metric: nonEmptyString(source.metric || source.metric_name || source.metricName || defaults.metric),
		unit: nonEmptyString(source.unit || defaults.unit),
	};
	const items = normalizeArray(rawItems).map((entry, index) => normalizeFuzzHotspotItem(entry, { ...summaryDefaults, rank: index + 1 })).filter(Boolean);
	if (items.length === 0) {
		return undefined;
	}
	items.sort((a, b) => a.rank - b.rank || b.relative_score - a.relative_score || String(a.surface_key).localeCompare(String(b.surface_key)));
	return stripUndefined({
		schema: WORDPRESS_FUZZ_HOTSPOT_SET_SCHEMA,
		version: 1,
		id: nonEmptyString(source.id || source.hotspot_set_id || source.hotspotSetId) || `${defaults.taskId || defaults.provider || 'wordpress'}-hotspots`,
		label: nonEmptyString(source.label),
		dimension: summaryDefaults.dimension,
		metric: summaryDefaults.metric,
		unit: summaryDefaults.unit,
		items,
		metadata: objectOrUndefined(source.metadata),
	});
}

function codeboxWordPressHotspotItems(source = {}) {
	const direct = source.hotspots || source.items || source.entries || source.summary;
	if (direct !== undefined) {
		return direct;
	}
	if (source.schema !== WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA) {
		return [];
	}
	return [
		...typedHotspotItems(source.database || source.db || source.queries || source.query_hotspots || source.queryHotspots, 'database'),
		...typedHotspotItems(source.api || source.rest || source.routes || source.rest_routes || source.restRoutes, 'api'),
		...typedHotspotItems(source.performance || source.timing || source.timings, 'performance'),
	];
}

function typedHotspotItems(value, dimension) {
	return normalizeArray(value).flatMap((entry) => {
		if (Array.isArray(entry)) {
			return typedHotspotItems(entry, dimension);
		}
		return objectOrUndefined(entry) ? [{ dimension, ...entry }] : [];
	});
}

function normalizeFuzzHotspotItem(entry = {}, defaults = {}) {
	if (!objectOrUndefined(entry)) {
		return undefined;
	}
	const metadata = objectOrUndefined(entry.metadata) || {};
	const surfaceKey = nonEmptyString(entry.surface_key || entry.surfaceKey || entry.surface || entry.surface_id || entry.surfaceId || entry.route || entry.endpoint || entry.path || entry.table || entry.query || metadata.surface_key || metadata.surfaceKey || entry.key || entry.id);
	const operationKey = nonEmptyString(entry.operation_key || entry.operationKey || entry.operation || entry.operation_id || entry.operationId || entry.method || entry.verb || entry.action || metadata.operation_key || metadata.operationKey || surfaceKey);
	const metric = nonEmptyString(entry.metric || entry.metric_name || entry.metricName || defaults.metric || inferMetric(entry));
	const value = numericValue(entry.value ?? entry.metric_value ?? entry.metricValue ?? entry.count ?? entry.total ?? entry.duration_ms ?? entry.durationMs);
	if (!surfaceKey || !operationKey || !metric || value === undefined) {
		return undefined;
	}
	const dimension = nonEmptyString(entry.dimension || entry.kind || defaults.dimension || 'performance');
	return stripUndefined({
		id: nonEmptyString(entry.id || entry.key) || `${surfaceKey}:${operationKey}:${metric}`,
		dimension,
		kind: nonEmptyString(entry.kind || entry.type),
		metric,
		value,
		unit: nonEmptyString(entry.unit || defaults.unit),
		basis: nonEmptyString(entry.basis || entry.normalization),
		rank: positiveInteger(entry.rank || entry.position || defaults.rank, defaults.rank || 1),
		relative_score: numericValue(entry.relative_score ?? entry.relativeScore ?? entry.score, 0),
		sample_count: positiveInteger(entry.sample_count || entry.sampleCount || entry.samples || entry.count, 1),
		evidence_refs: normalizeEvidenceRefs(entry.evidence_refs || entry.evidenceRefs || entry.evidence),
		artifact_refs: normalizeEvidenceRefs(entry.artifacts || entry.artifact_refs || entry.artifactRefs),
		metadata: stripUndefined({
			...metadata,
			surface_key: surfaceKey,
			operation_key: operationKey,
		}),
	});
}

function inferMetric(entry = {}) {
	if (entry.totalTimeMs !== undefined || entry.total_time_ms !== undefined || entry.sampleMs !== undefined || entry.sample_ms !== undefined) {
		return 'query_time_ms';
	}
	if (entry.rows !== undefined) {
		return 'rows';
	}
	if (entry.sets !== undefined) {
		return 'cache_sets';
	}
	if (entry.deletes !== undefined) {
		return 'cache_deletes';
	}
	if (entry.hits !== undefined) {
		return 'cache_hits';
	}
	if (entry.misses !== undefined) {
		return 'cache_misses';
	}
	if (entry.duration_ms !== undefined || entry.durationMs !== undefined) {
		return 'duration_ms';
	}
	if (entry.query_count !== undefined || entry.queryCount !== undefined) {
		return 'query_count';
	}
	if (entry.memory_peak_bytes !== undefined || entry.memoryPeakBytes !== undefined) {
		return 'memory_peak_bytes';
	}
	return 'value';
}

function normalizeEvidenceRefs(value) {
	return normalizeArray(value).map((ref) => {
		if (typeof ref === 'string') {
			return ref;
		}
		if (!objectOrUndefined(ref)) {
			return undefined;
		}
		return nonEmptyString(ref.ref || ref.id || ref.path || ref.url);
	}).filter(Boolean);
}

function normalizeProvider(value) {
	if (objectOrUndefined(value)) {
		return stripUndefined({
			id: nonEmptyString(value.id || value.provider || value.runtime) || 'wp-codebox',
			name: nonEmptyString(value.name),
			schema: nonEmptyString(value.schema),
			metadata: objectOrUndefined(value.metadata),
		});
	}
	return { id: nonEmptyString(value) || 'wp-codebox' };
}

function normalizeStatus(value) {
	const status = String(value || '').trim().toLowerCase();
	if (['success', 'passed', 'ok'].includes(status)) {
		return 'succeeded';
	}
	if (['failure', 'error'].includes(status)) {
		return 'failed';
	}
	return status || 'skipped';
}

function succeededFromStatus(status) {
	if (['failed', 'errored'].includes(status)) {
		return false;
	}
	if (['succeeded', 'skipped', 'partial'].includes(status)) {
		return status === 'succeeded';
	}
	return undefined;
}

function positiveInteger(value, fallback) {
	const number = Number(value);
	return Number.isInteger(number) && number > 0 ? number : fallback;
}

function numericValue(value, fallback) {
	const number = Number(value);
	return Number.isFinite(number) ? number : fallback;
}

function booleanOrUndefined(value) {
	if (value === true || value === false) {
		return value;
	}
	if (value === 'yes' || value === 'true' || value === '1') {
		return true;
	}
	if (value === 'no' || value === 'false' || value === '0') {
		return false;
	}
	return undefined;
}

function nonEmptyString(value) {
	return typeof value === 'string' && value.trim() !== '' ? value.trim() : undefined;
}

function normalizeArray(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return Array.isArray(value) ? value.filter(Boolean) : [value].filter(Boolean);
}

function objectOrUndefined(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

function stripUndefined(value) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return value;
	}
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
	WORDPRESS_FUZZ_HOTSPOT_SUMMARY_SCHEMA,
	WORDPRESS_FUZZ_RUNTIME_TASK_REQUEST_SCHEMA,
	WORDPRESS_FUZZ_RUNTIME_TASK_RESULT_SCHEMA,
	WORDPRESS_FUZZ_HOTSPOT_SET_SCHEMA,
	WORDPRESS_FUZZ_OBSERVATION_SET_SCHEMA,
	WP_CODEBOX_WORDPRESS_HOTSPOTS_SCHEMA,
	WP_CODEBOX_WORDPRESS_OBSERVATIONS_SCHEMA,
	buildWordPressFuzzRuntimeTaskRequest,
	fuzzHotspotSummaryFromObservationSet,
	normalizeFuzzObservationSet,
	normalizeFuzzHotspotSummary,
	normalizeWordPressFuzzRuntimeTaskResult,
};
