'use strict';

/* eslint-disable camelcase */

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

const FIXTURE_WORKLOAD_MATRIX_SCHEMA = 'homeboy/fixture-workload-matrix/v1';
const FIXTURE_WORKLOAD_MATRIX_RESULT_SCHEMA = 'homeboy/fixture-workload-matrix-result/v1';
const FIXTURE_ARTIFACT_SCHEMA = 'homeboy/fixture-artifact/v1';

function discoverFixtureWorkloads(root, options = {}) {
	const fixtureRoot = requiredDirectory(root || options.fixtureRoot || options.fixture_root, 'fixtureRoot');
	const entrypoint = options.entrypoint || 'index.html';
	const maxDepth = integerOption(options.maxDepth || options.max_depth, 2);
	const fixtures = [];

	visitFixtureDirectory(fixtureRoot, 0, maxDepth, (directory) => {
		const entryPath = path.join(directory, entrypoint);
		if (!fs.existsSync(entryPath) || !fs.statSync(entryPath).isFile()) {
			return;
		}

		fixtures.push(normalizeFixture({ root: fixtureRoot, directory, entrypoint }));
	});

	return fixtures.sort((left, right) => left.id.localeCompare(right.id));
}

function createFixtureWorkloadMatrix(input = {}) {
	const fixtures = normalizeFixtures(input.fixtures || discoverFixtureWorkloads(input.fixture_root || input.fixtureRoot, input));
	const batches = createFixtureBatches(fixtures, input);
	return {
		schema: FIXTURE_WORKLOAD_MATRIX_SCHEMA,
		id: input.id || input.run_id || input.runId || 'fixture-workload-matrix',
		fixture_root: input.fixture_root || input.fixtureRoot || commonFixtureRoot(fixtures),
		entrypoint: input.entrypoint || 'index.html',
		count: fixtures.length,
		batch_count: batches.length,
		fixtures,
		batches,
		artifacts: {
			matrix: input.matrix_artifact || input.matrixArtifact || 'matrix.json',
			result: input.result_artifact || input.resultArtifact || 'fixture-workload-matrix-result.json',
			summary: input.summary_artifact || input.summaryArtifact || 'summary.json',
			diagnostics: input.diagnostics_artifact || input.diagnosticsArtifact || 'diagnostic-packets.json',
		},
		metadata: input.metadata || {},
	};
}

function buildFixtureArtifact(fixture, options = {}) {
	const normalized = normalizeFixture(fixture);
	const files = collectFixtureFiles(normalized.directory, options);
	const prefix = String(options.artifactPrefix || options.artifact_prefix || 'fixture').replace(/^\/+|\/+$/g, '') || 'fixture';
	const entryPath = `${prefix}/${normalized.entrypoint}`;
	return {
		schema: FIXTURE_ARTIFACT_SCHEMA,
		fixture_id: normalized.id,
		entrypoint: entryPath,
		entry_path: entryPath,
		files: files.map((file) => fixtureArtifactFile(file, prefix)),
		summary: {
			file_count: files.length,
			entry_path: entryPath,
			total_bytes: files.reduce((sum, file) => sum + file.bytes, 0),
		},
		source_metadata: {
			fixture_id: normalized.id,
			fixture_path: normalized.directory,
			fixture_entrypoint: normalized.entrypoint,
		},
	};
}

function buildWpCodeboxFixtureWorkloadMatrixRecipe(input = {}) {
	const matrix = input.matrix || createFixtureWorkloadMatrix(input);
	const artifactsDirectory = input.artifactsDirectory || input.artifacts_directory || '/artifacts/fixture-workload-matrix';
	const playgroundArtifactsDirectory = input.playgroundArtifactsDirectory || input.playground_artifacts_directory;
	const commandArtifactsDirectory = playgroundArtifactsDirectory || artifactsDirectory;
	const mounts = normalizeArray(input.mounts);
	if (playgroundArtifactsDirectory) {
		mounts.push({ source: artifactsDirectory, target: playgroundArtifactsDirectory, mode: 'readwrite' });
	}

	const setupSteps = [
		...pluginActivationSteps(input.pluginActivations || input.plugin_activations),
		...normalizeArray(input.setupSteps || input.setup_steps),
	];
	const workloadSteps = matrix.fixtures.map((fixture) => renderWorkloadStep({
		fixture,
		artifactPath: artifactPathForFixture(fixture, commandArtifactsDirectory),
		input,
	}));

	return {
		schema: 'wp-codebox/workspace-recipe/v1',
		runtime: {
			wp: input.wordpressVersion || input.wordpress_version || 'latest',
			blueprint: input.blueprint || {},
		},
		inputs: compactObject({
			mounts,
			extra_plugins: normalizeArray(input.extra_plugins),
		}),
		workflow: { steps: [...setupSteps, ...workloadSteps] },
		artifacts: { directory: artifactsDirectory },
	};
}

function normalizeFixtureWorkloadMatrixResult(input = {}) {
	const matrix = input.matrix || createFixtureWorkloadMatrix(input);
	const compareFixtureResult = typeof input.compareFixtureResult === 'function' ? input.compareFixtureResult : input.compare_fixture_result;
	const results = normalizeArray(input.results || input.fixture_results || input.fixtureResults).map((result) => normalizeFixtureResult(result));
	const resultByFixture = new Map(results.map((result) => [result.fixture_id, result]));
	const fixtureResults = matrix.fixtures.map((fixture) => {
		const result = resultByFixture.get(fixture.id) || normalizeFixtureResult({ fixture_id: fixture.id, status: 'not_run' });
		return typeof compareFixtureResult === 'function' ? normalizeFixtureResult({ ...result, comparison: compareFixtureResult(result, fixture, matrix) }) : result;
	});
	const diagnostics = dedupeDiagnosticPackets(fixtureResults.flatMap((result) => diagnosticPacketsForFixtureResult(result, { matrix })));
	const grouped = groupDiagnosticPackets(diagnostics);
	const summaryLimit = boundedSummaryLimit(input);

	return {
		schema: FIXTURE_WORKLOAD_MATRIX_RESULT_SCHEMA,
		matrix_id: matrix.id,
		fixture_root: matrix.fixture_root,
		summary: {
			fixture_count: matrix.fixtures.length,
			succeeded: fixtureResults.filter((result) => result.status === 'passed').length,
			failed: fixtureResults.filter((result) => result.status === 'failed').length,
			not_run: fixtureResults.filter((result) => result.status === 'not_run').length,
			diagnostic_count: diagnostics.length,
			groups: Object.fromEntries(Object.entries(grouped).map(([key, items]) => [key, items.length])),
			failure_summaries: projectFailureSummaries(fixtureResults, diagnostics, summaryLimit),
			top_diagnostic_kinds: topDiagnosticValues(diagnostics, 'kind', summaryLimit),
			top_fixtures_by_finding_count: topFixtureFindingCounts(diagnostics, summaryLimit),
			top_severities: topDiagnosticValues(diagnostics, 'severity', summaryLimit),
			top_categories: topDiagnosticValues(diagnostics, 'category', summaryLimit),
			top_runtime_target_selectors: topDiagnosticValues(diagnostics, 'selector', summaryLimit),
			top_core_html_sources: topCoreHtmlSources(diagnostics, summaryLimit),
		},
		fixtures: fixtureResults,
		diagnostics,
		fanout_groups: Object.entries(grouped).map(([group_key, items], index) => ({ key: group_key, index, diagnostics: items })),
	};
}

function projectFailureSummaries(fixtureResults, diagnostics, limit) {
	const resultsByFixture = new Map(fixtureResults.map((result) => [result.fixture_id, result]));
	return diagnostics
		.filter((diagnostic) => ['failed', 'not_run'].includes(resultsByFixture.get(diagnostic.fixture_id)?.status))
		.map((diagnostic) => {
			const result = resultsByFixture.get(diagnostic.fixture_id) || {};
			return {
				fixture_id: diagnostic.fixture_id,
				status: result.status,
				kind: diagnostic.kind,
				category: diagnostic.category,
				severity: diagnostic.severity,
				reason: diagnostic.reason,
				artifact_refs: dedupeArtifactRefs([...normalizeArray(diagnostic.artifact_refs), ...normalizeArray(result.artifact_refs)]).slice(0, 5),
				retryable: retryability(diagnostic, result),
			};
		})
		.sort((left, right) => `${left.fixture_id}\u0000${left.category}\u0000${left.reason}`.localeCompare(`${right.fixture_id}\u0000${right.category}\u0000${right.reason}`))
		.slice(0, limit);
}

function retryability(diagnostic, result) {
	for (const value of [diagnostic.raw?.retryable, diagnostic.raw?.retry, result.raw?.retryable, result.raw?.retry, result.retryable, result.retry]) {
		if (typeof value === 'boolean') {
			return value;
		}
	}
	return null;
}

function topDiagnosticValues(diagnostics, key, limit) {
	return topCounts(diagnostics.map((diagnostic) => diagnostic[key]).filter(Boolean), limit);
}

function topFixtureFindingCounts(diagnostics, limit) {
	return topCounts(diagnostics.map((diagnostic) => diagnostic.fixture_id).filter(Boolean), limit).map(({ value, count }) => ({ fixture_id: value, finding_count: count }));
}

function topCoreHtmlSources(diagnostics, limit) {
	return topCounts(diagnostics
		.filter((diagnostic) => ['core/html', 'core-html'].includes(String(diagnostic.raw?.block_name || diagnostic.raw?.blockName || diagnostic.raw?.source_block || diagnostic.raw?.sourceBlock || '').toLowerCase()))
		.map((diagnostic) => diagnostic.raw?.element || diagnostic.raw?.tag || diagnostic.raw?.html_element || diagnostic.raw?.htmlElement || diagnostic.source_path)
		.filter(Boolean), limit);
}

function topCounts(values, limit) {
	const counts = new Map();
	for (const value of values) {
		const normalized = String(value).trim();
		if (normalized) {
			counts.set(normalized, (counts.get(normalized) || 0) + 1);
		}
	}
	return Array.from(counts, ([value, count]) => ({ value, count }))
		.sort((left, right) => right.count - left.count || left.value.localeCompare(right.value))
		.slice(0, limit);
}

function boundedSummaryLimit(input) {
	return Math.min(integerOption(input.summaryLimit || input.summary_limit || input.failureSummaryLimit || input.failure_summary_limit, 20), 50);
}

function collectFixtureWorkloadMatrixRunResults(input = {}) {
	const matrix = input.matrix || createFixtureWorkloadMatrix(input);
	const outputDirectory = requiredString(input.outputDirectory || input.output_directory, 'outputDirectory');
	const codeboxOutput = input.codeboxOutput || input.codebox_output || readJsonFileIfExists(input.outputFile || input.output_file) || null;
	const codeboxError = input.codeboxError || input.codebox_error || null;
	const runtimePayloads = collectRuntimePayloads(codeboxOutput);
	const resultFileNames = normalizeArray(input.resultFileNames || input.result_file_names || ['result.json', 'validation-result.json', 'diagnostics.json']);
	const results = matrix.fixtures.map((fixture) => {
		const fixtureArtifactsDirectory = path.join(outputDirectory, fixture.id);
		const payloads = [
			...runtimePayloads.filter((payload) => payloadMatchesFixture(payload, fixture)),
			...readFixturePayloadFiles(fixtureArtifactsDirectory, resultFileNames),
		];
		return normalizeCollectedFixtureResult({ fixture, payloads, fixtureArtifactsDirectory, codeboxError });
	});

	return normalizeFixtureWorkloadMatrixResult({ ...input, matrix, results });
}

function writeFixtureWorkloadMatrixResultArtifacts(input = {}) {
	const outputDirectory = requiredString(input.outputDirectory || input.output_directory, 'outputDirectory');
	const matrix = input.matrix || createFixtureWorkloadMatrix(input);
	const result = input.result || normalizeFixtureWorkloadMatrixResult({ ...input, matrix });

	fs.mkdirSync(outputDirectory, { recursive: true });
	writeJsonFile(path.join(outputDirectory, matrix.artifacts.result), result);
	writeJsonFile(path.join(outputDirectory, matrix.artifacts.summary), result.summary);
	writeJsonFile(path.join(outputDirectory, matrix.artifacts.diagnostics), result.diagnostics);

	return result;
}

function writeFixtureWorkloadMatrixArtifacts(input = {}) {
	const outputDirectory = requiredString(input.outputDirectory || input.output_directory, 'outputDirectory');
	const matrix = input.matrix || createFixtureWorkloadMatrix(input);
	const result = input.result || normalizeFixtureWorkloadMatrixResult({ ...input, matrix });

	fs.mkdirSync(outputDirectory, { recursive: true });
	for (const fixture of matrix.fixtures) {
		const fixtureDirectory = path.join(outputDirectory, fixture.id);
		fs.mkdirSync(fixtureDirectory, { recursive: true });
		writeJsonFile(path.join(fixtureDirectory, 'artifact.json'), buildFixtureArtifact(fixture, input));
	}

	writeJsonFile(path.join(outputDirectory, matrix.artifacts.matrix), matrix);
	writeFixtureWorkloadMatrixResultArtifacts({ outputDirectory, matrix, result });

	return {
		matrix,
		result,
		artifact_refs: [
			artifactRef('matrix', path.join(outputDirectory, matrix.artifacts.matrix), 'matrix'),
			artifactRef('result', path.join(outputDirectory, matrix.artifacts.result), 'diagnostic'),
			artifactRef('summary', path.join(outputDirectory, matrix.artifacts.summary), 'summary'),
			artifactRef('diagnostic-packets', path.join(outputDirectory, matrix.artifacts.diagnostics), 'diagnostic'),
		],
	};
}

function createFixtureBatches(fixtures, input = {}) {
	const batchSize = integerOption(input.batchSize || input.batch_size, fixtures.length || 1);
	const batches = [];
	for (let index = 0; index < fixtures.length; index += batchSize) {
		const batchFixtures = fixtures.slice(index, index + batchSize);
		batches.push({
			id: `${input.batchPrefix || input.batch_prefix || 'batch'}-${batches.length + 1}`,
			index: batches.length,
			fixture_ids: batchFixtures.map((fixture) => fixture.id),
			count: batchFixtures.length,
		});
	}
	return batches;
}

function renderWorkloadStep({ fixture, artifactPath, input }) {
	const context = {
		fixture,
		fixture_id: fixture.id,
		fixture_label: fixture.label,
		fixture_path: fixture.fixture_path,
		artifact_path: artifactPath,
	};
	const template = input.workloadStep || input.workload_step || input.stepTemplate || input.step_template;
	if (template) {
		return renderTemplateObject(template, context);
	}
	const command = input.command || input.workloadCommand || input.workload_command || 'wordpress.wp-cli';
	const args = normalizeArray(input.argsTemplate || input.args_template || input.args || input.commandArgs || input.command_args);
	if (args.length === 0) {
		throw new TypeError('A workload step template or command args must be provided.');
	}
	return { command, args: args.map((arg) => renderTemplateString(arg, context)) };
}

function pluginActivationSteps(activations) {
	return normalizeArray(activations).map((activation) => {
		if (typeof activation === 'string') {
			return { command: 'wordpress.wp-cli', args: [`command=plugin activate ${activation}`] };
		}
		return {
			command: activation.command || 'wordpress.wp-cli',
			args: normalizeArray(activation.args || [`command=plugin activate ${requiredString(activation.plugin || activation.plugin_file || activation.pluginFile, 'plugin activation file')}`]),
		};
	});
}

function diagnosticPacketsForFixtureResult(result, context = {}) {
	const diagnostics = normalizeArray(result.diagnostics || result.findings || result.messages);
	const packets = diagnostics.map((diagnostic, index) => normalizeDiagnosticPacket(diagnostic, result, index));
	if (result.status === 'failed' && packets.length === 0) {
		packets.push(normalizeDiagnosticPacket({ kind: 'fixture_failed', message: result.error || 'Fixture workload failed without a structured diagnostic.' }, result, 0));
	}
	if (context.matrix?.fixtures?.some((fixture) => fixture.id === result.fixture_id) && result.status === 'not_run') {
		packets.push(normalizeDiagnosticPacket({ kind: 'fixture_not_run', message: 'Fixture was discovered but did not produce a result.' }, result, 0));
	}
	return packets;
}

function normalizeDiagnosticPacket(diagnostic, result, index) {
	const raw = diagnostic && typeof diagnostic === 'object' ? diagnostic : { message: String(diagnostic || '') };
	const message = raw.message || raw.reason || raw.detail || raw.code || result.error || '';
	const group = raw.group_key || raw.groupKey || raw.category || raw.kind || raw.code || 'diagnostic';
	return {
		id: raw.id || `${result.fixture_id || 'fixture'}:${group}:${index + 1}`,
		kind: raw.kind || raw.code || 'diagnostic',
		category: raw.category || group,
		group_key: group,
		severity: raw.severity || (result.status === 'failed' ? 'error' : 'warning'),
		fixture_id: result.fixture_id || '',
		path: raw.path || raw.source_path || result.fixture_path || '',
		source_path: raw.source_path || raw.path || result.fixture_path || '',
		selector: raw.selector || raw.context?.selector || raw.runtime_target_selector || '',
		reason: message,
		repair_mode: raw.repair_mode || raw.repairMode || raw.suggested_repair_class || '',
		candidate_repo: raw.candidate_repo || raw.candidateRepo || raw.parser_owner || raw.owner || '',
		artifact_refs: normalizeArray(raw.artifact_refs || raw.artifactRefs),
		raw,
	};
}

function dedupeDiagnosticPackets(diagnostics) {
	const byKey = new Map();
	for (const diagnostic of diagnostics) {
		const key = diagnosticDedupeKey(diagnostic);
		if (!key) {
			byKey.set(`packet:${byKey.size}`, diagnostic);
			continue;
		}

		const existing = byKey.get(key);
		byKey.set(key, existing ? mergeDuplicateDiagnosticPacket(existing, diagnostic) : diagnostic);
	}
	return Array.from(byKey.values());
}

function diagnosticDedupeKey(diagnostic) {
	const reason = firstString([diagnostic.reason, diagnostic.raw?.reason, diagnostic.raw?.message, diagnostic.raw?.code]);
	if (!reason) {
		return '';
	}

	const locator = firstString([
		diagnostic.selector,
		diagnostic.raw?.selector,
		diagnostic.raw?.context?.selector,
		diagnostic.raw?.runtime_target_selector,
		diagnostic.source_path,
		diagnostic.path,
	]);
	if (!locator) {
		return '';
	}

	return [
		diagnostic.fixture_id || '',
		diagnostic.source_path || diagnostic.path || '',
		diagnostic.path || diagnostic.source_path || '',
		locator,
		reason,
	].map((value) => String(value || '').trim().toLowerCase()).join('\u0000');
}

function mergeDuplicateDiagnosticPacket(left, right) {
	const canonical = diagnosticCanonicalScore(right) > diagnosticCanonicalScore(left) ? right : left;
	const duplicate = canonical === left ? right : left;
	return {
		...canonical,
		artifact_refs: dedupeArtifactRefs([...normalizeArray(left.artifact_refs), ...normalizeArray(right.artifact_refs)]),
		duplicate_diagnostic_ids: Array.from(new Set([
			...normalizeArray(left.duplicate_diagnostic_ids),
			...normalizeArray(right.duplicate_diagnostic_ids),
			duplicate.id,
		].filter(Boolean))),
	};
}

function diagnosticCanonicalScore(diagnostic) {
	const kind = String(diagnostic.kind || '').trim();
	const reason = String(diagnostic.reason || diagnostic.raw?.reason || '').trim();
	let score = severityScore(diagnostic.severity) * 100;
	if (kind && kind !== 'diagnostic') {
		score += 10;
	}
	if (kind && reason && compactSlug(kind) !== compactSlug(reason)) {
		score += 5;
	}
	if (diagnostic.repair_mode) {
		score += 3;
	}
	if (diagnostic.candidate_repo) {
		score += 2;
	}
	if (diagnostic.group_key && diagnostic.raw?.group_key && diagnostic.group_key === diagnostic.raw.group_key) {
		score += 1;
	}
	return score;
}

function severityScore(severity) {
	return { error: 3, fatal: 3, warning: 2, warn: 2, notice: 1, info: 1 }[String(severity || '').toLowerCase()] || 0;
}

function dedupeArtifactRefs(refs) {
	const seen = new Set();
	const deduped = [];
	for (const ref of refs) {
		const key = JSON.stringify(ref || null);
		if (!seen.has(key)) {
			seen.add(key);
			deduped.push(ref);
		}
	}
	return deduped;
}

function normalizeFixture(input) {
	const directory = requiredDirectory(input.directory || input.path || input.fixture_path || input.fixturePath, 'fixture.directory');
	const root = input.root || input.fixture_root || input.fixtureRoot || path.dirname(directory);
	const relative = path.relative(path.resolve(root), path.resolve(directory));
	const id = slug(input.id || input.slug || (relative && !relative.startsWith('..') ? relative : path.basename(directory)));
	return { id, label: input.label || input.name || id, directory, fixture_path: directory, fixture_root: root, entrypoint: input.entrypoint || 'index.html', metadata: input.metadata || {} };
}

function normalizeFixtures(fixtures) {
	return normalizeArray(fixtures).map((fixture) => normalizeFixture(fixture));
}

function normalizeFixtureResult(input = {}) {
	let status = input.status || 'not_run';
	if (!input.status && input.success === true) {
		status = 'passed';
	} else if (!input.status && input.success === false) {
		status = 'failed';
	}
	return {
		fixture_id: input.fixture_id || input.fixtureId || input.id || '',
		fixture_path: input.fixture_path || input.fixturePath || input.path || '',
		status,
		success: status === 'passed',
		error: input.error || input.message || '',
		diagnostics: normalizeArray(input.diagnostics || input.findings || input.messages),
		artifact_refs: normalizeArray(input.artifact_refs || input.artifactRefs),
		artifacts: input.artifacts || {},
		metrics: input.metrics || {},
		comparison: input.comparison || null,
		raw: input.raw || input,
	};
}

function normalizeCollectedFixtureResult({ fixture, payloads, fixtureArtifactsDirectory, codeboxError }) {
	const merged = mergeObjects(payloads);
	const diagnostics = collectDiagnostics(merged);
	const artifactRefs = collectArtifactRefs(merged, fixtureArtifactsDirectory);
	const error = firstString([merged.error, merged.message && isFailurePayload(merged) ? merged.message : '', codeboxError && payloads.length === 0 ? codeboxError.message || String(codeboxError) : '']);
	const success = inferFixtureSuccess(merged, diagnostics, error, payloads.length);
	return normalizeFixtureResult({
		fixture_id: fixture.id,
		fixture_path: fixture.fixture_path,
		status: fixtureStatus(payloads.length, error, success),
		success,
		error,
		diagnostics,
		artifact_refs: artifactRefs,
		artifacts: merged.artifacts || {},
		metrics: merged.metrics || merged.summary || {},
		raw: { payloads },
	});
}

function collectDiagnostics(payload) {
	return [
		...normalizeArray(payload.diagnostics),
		...normalizeArray(payload.findings),
		...normalizeArray(payload.messages),
		...normalizeArray(payload.errors),
		...normalizeArray(payload.warnings),
	];
}

function collectArtifactRefs(payload, fixtureArtifactsDirectory) {
	const refs = [...normalizeArray(payload.artifact_refs || payload.artifactRefs), ...normalizeArray(payload.artifacts?.refs)];
	for (const [key, value] of Object.entries(payload.artifacts || {})) {
		if (value && typeof value === 'object' && !Array.isArray(value) && (value.path || value.file || value.href)) {
			refs.push({ artifact_id: key, kind: value.kind || key, ...value });
		} else if (typeof value === 'string') {
			refs.push({ artifact_id: key, kind: key, path: value });
		}
	}
	for (const fileName of ['artifact.json', 'result.json', 'validation-result.json', 'diagnostics.json']) {
		const filePath = path.join(fixtureArtifactsDirectory, fileName);
		if (fs.existsSync(filePath)) {
			refs.push(artifactRef(fileName.replace(/\.json$/, ''), filePath, fileName === 'artifact.json' ? 'input' : 'diagnostic'));
		}
	}
	return refs;
}

function collectRuntimePayloads(value) {
	const payloads = [];
	visitRuntimePayloads(value, '', payloads, new Set());
	return payloads;
}

function visitRuntimePayloads(value, inheritedFixtureId, payloads, seen) {
	if (!value || typeof value !== 'object' || seen.has(value)) {
		return;
	}
	seen.add(value);
	const fixtureId = fixtureIdentity(value) || inheritedFixtureId;
	if (fixtureId && hasPayloadData(value)) {
		payloads.push({ fixture_id: fixtureId, ...value });
	}
	for (const key of ['stdout', 'stderr', 'output', 'result']) {
		for (const parsed of parseJsonPayloadsFromText(value[key])) {
			payloads.push({ fixture_id: fixtureId, ...parsed });
		}
	}
	for (const child of Array.isArray(value) ? value : Object.values(value)) {
		visitRuntimePayloads(child, fixtureId, payloads, seen);
	}
}

function hasPayloadData(value) {
	return ['status', 'success', 'ok', 'passed', 'error', 'diagnostics', 'findings', 'summary', 'artifacts'].some((key) => Object.hasOwn(value, key));
}

function readFixturePayloadFiles(directory, fileNames) {
	return fileNames.map((fileName) => readJsonFileIfExists(path.join(directory, fileName))).filter(Boolean);
}

function payloadMatchesFixture(payload, fixture) {
	return fixtureIdentity(payload) === fixture.id;
}

function fixtureIdentity(payload) {
	return payload?.fixture_id || payload?.fixtureId || payload?.fixture?.id || payload?.fixture?.slug || payload?.request?.fixture_id || payload?.request?.fixtureId || payload?.metadata?.fixture_id || payload?.metadata?.fixtureId || '';
}

function inferFixtureSuccess(payload, diagnostics, error, payloadCount) {
	if (payload.success === true || payload.ok === true || payload.passed === true) {
		return diagnostics.length === 0 && !error;
	}
	if (payload.success === false || payload.ok === false || payload.passed === false || payload.status === 'failed' || payload.status === 'error') {
		return false;
	}
	if (payload.status === 'passed' || payload.status === 'success') {
		return diagnostics.length === 0 && !error;
	}
	return payloadCount > 0 && diagnostics.length === 0 && !error;
}

function fixtureStatus(payloadCount, error, success) {
	if (payloadCount === 0 && !error) {
		return 'not_run';
	}
	return success ? 'passed' : 'failed';
}

function isFailurePayload(payload) {
	return payload.success === false || payload.ok === false || payload.status === 'failed' || payload.status === 'error';
}

function collectFixtureFiles(directory, options = {}) {
	const maxFiles = integerOption(options.maxFiles || options.max_files, 1000);
	const files = [];
	const visit = (current) => {
		for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
			if (entry.name === '.git' || entry.name === 'node_modules') {
				continue;
			}
			const entryPath = path.join(current, entry.name);
			if (entry.isDirectory()) {
				visit(entryPath);
				continue;
			}
			if (!entry.isFile()) {
				continue;
			}
			const relativePath = path.relative(directory, entryPath).replace(/\\/g, '/');
			const stat = fs.statSync(entryPath);
			files.push({ relative_path: relativePath, absolute_path: entryPath, type: fileType(relativePath), bytes: stat.size });
			if (files.length > maxFiles) {
				throw new Error(`Fixture ${directory} has more than ${maxFiles} files.`);
			}
		}
	};
	visit(directory);
	return files.sort((left, right) => left.relative_path.localeCompare(right.relative_path));
}

function fixtureArtifactFile(file, prefix) {
	const artifactFile = { path: `${prefix}/${file.relative_path}`, source_path: file.absolute_path, type: file.type, bytes: file.bytes };
	const payload = fs.readFileSync(file.absolute_path);
	if (isTextPayloadType(file.type)) {
		artifactFile.content = payload.toString('utf8');
	} else {
		artifactFile.content_base64 = payload.toString('base64');
	}
	return artifactFile;
}

function visitFixtureDirectory(directory, depth, maxDepth, callback) {
	callback(directory);
	if (depth >= maxDepth) {
		return;
	}
	for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
		if (entry.isDirectory() && entry.name !== '.git' && entry.name !== 'node_modules') {
			visitFixtureDirectory(path.join(directory, entry.name), depth + 1, maxDepth, callback);
		}
	}
}

function artifactPathForFixture(fixture, artifactsDirectory) {
	return path.join(artifactsDirectory, fixture.id, 'artifact.json');
}

function groupDiagnosticPackets(diagnostics) {
	return diagnostics.reduce((groups, diagnostic) => {
		const key = diagnostic.group_key || 'diagnostic';
		groups[key] = groups[key] || [];
		groups[key].push(diagnostic);
		return groups;
	}, {});
}

function fileType(filePath) {
	const extension = path.extname(filePath).toLowerCase();
	if (extension === '.html' || extension === '.htm') {
		return 'text/html';
	}
	if (extension === '.css') {
		return 'text/css';
	}
	if (extension === '.js' || extension === '.mjs') {
		return 'application/javascript';
	}
	if (extension === '.json') {
		return 'application/json';
	}
	return 'application/octet-stream';
}

function isTextPayloadType(type) {
	return typeof type === 'string' && (type.startsWith('text/') || type === 'application/javascript' || type === 'application/json');
}

function commonFixtureRoot(fixtures) {
	return fixtures[0]?.fixture_root || '';
}

function normalizeArray(value) {
	if (Array.isArray(value)) {
		return value;
	}
	if (value === undefined || value === null || value === '') {
		return [];
	}
	return [value];
}

function mergeObjects(values) {
	return values.reduce((merged, value) => deepMerge(merged, value && typeof value === 'object' && !Array.isArray(value) ? value : {}), {});
}

function deepMerge(left, right) {
	const output = { ...left };
	for (const [key, value] of Object.entries(right)) {
		if (Array.isArray(value)) {
			output[key] = [...normalizeArray(output[key]), ...value];
		} else if (value && typeof value === 'object' && !Array.isArray(value) && output[key] && typeof output[key] === 'object' && !Array.isArray(output[key])) {
			output[key] = deepMerge(output[key], value);
		} else if (value !== undefined && value !== null && value !== '') {
			output[key] = value;
		}
	}
	return output;
}

function compactObject(value) {
	return Object.fromEntries(Object.entries(value || {}).filter(([, item]) => item !== undefined && item !== null && item !== '' && (!Array.isArray(item) || item.length > 0)));
}

function firstString(values) {
	return values.map((value) => String(value || '').trim()).find(Boolean) || '';
}

function readJsonFileIfExists(filePath) {
	if (!filePath || !fs.existsSync(filePath) || !fs.statSync(filePath).isFile()) {
		return null;
	}
	try {
		return JSON.parse(fs.readFileSync(filePath, 'utf8'));
	} catch (error) {
		return { status: 'failed', error: `Unable to parse JSON artifact ${filePath}: ${error.message}`, artifact_refs: [artifactRef('unparseable-json', filePath, 'diagnostic')] };
	}
}

function parseJsonPayloadsFromText(text) {
	if (typeof text !== 'string' || !text.trim()) {
		return [];
	}
	const payloads = [];
	const trimmed = text.trim();
	const candidates = new Set([trimmed, ...text.split(/\r?\n/).map((line) => line.trim())]);
	const firstObject = trimmed.indexOf('{');
	const lastObject = trimmed.lastIndexOf('}');
	if (firstObject >= 0 && lastObject > firstObject) {
		candidates.add(trimmed.slice(firstObject, lastObject + 1));
	}
	for (const candidate of candidates) {
		if (!candidate || !candidate.startsWith('{')) {
			continue;
		}
		try {
			const parsed = JSON.parse(candidate);
			if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
				payloads.push(parsed);
			}
		} catch {
			// Runtime output may mix human text and JSON; non-JSON lines are ignored.
		}
	}
	return payloads;
}

function renderTemplateObject(value, context) {
	if (Array.isArray(value)) {
		return value.map((item) => renderTemplateObject(item, context));
	}
	if (value && typeof value === 'object') {
		return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, renderTemplateObject(item, context)]));
	}
	return typeof value === 'string' ? renderTemplateString(value, context) : value;
}

function renderTemplateString(value, context) {
	return value.replace(/\{\{\s*([^}]+)\s*\}\}/g, (_match, key) => String(getPath(context, key.trim()) ?? ''));
}

function getPath(value, pathExpression) {
	return String(pathExpression).split('.').filter(Boolean).reduce((current, part) => current?.[part], value);
}

function integerOption(value, fallback) {
	const number = Number(value);
	return Number.isFinite(number) && number > 0 ? Math.floor(number) : fallback;
}

function requiredString(value, name) {
	if (typeof value !== 'string' || value.trim() === '') {
		throw new TypeError(`${name} must be a non-empty string.`);
	}
	return value;
}

function requiredDirectory(value, name) {
	const directory = requiredString(value, name);
	if (!fs.existsSync(directory) || !fs.statSync(directory).isDirectory()) {
		throw new Error(`${name} must be an existing directory: ${directory}`);
	}
	return path.resolve(directory);
}

function writeJsonFile(filePath, payload) {
	fs.writeFileSync(filePath, `${JSON.stringify(payload, null, 2)}\n`);
}

function artifactRef(artifact_id, filePath, kind) {
	return { schema: 'homeboy/artifact-ref/v1', artifact_id, kind, path: filePath };
}

function slug(value) {
	return String(value || 'fixture').toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '') || 'fixture';
}

function compactSlug(value) {
	return String(value || '').toLowerCase().replace(/[^a-z0-9]+/g, '');
}

module.exports = {
	FIXTURE_ARTIFACT_SCHEMA,
	FIXTURE_WORKLOAD_MATRIX_RESULT_SCHEMA,
	FIXTURE_WORKLOAD_MATRIX_SCHEMA,
	buildFixtureArtifact,
	buildWpCodeboxFixtureWorkloadMatrixRecipe,
	collectFixtureWorkloadMatrixRunResults,
	createFixtureBatches,
	createFixtureWorkloadMatrix,
	discoverFixtureWorkloads,
	normalizeFixtureWorkloadMatrixResult,
	writeFixtureWorkloadMatrixArtifacts,
	writeFixtureWorkloadMatrixResultArtifacts,
};
