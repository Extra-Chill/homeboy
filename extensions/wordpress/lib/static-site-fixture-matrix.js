'use strict';

/* eslint-disable camelcase */

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

const FIXTURE_MATRIX_SCHEMA = 'homeboy/static-site-fixture-matrix/v1';
const FIXTURE_MATRIX_RESULT_SCHEMA = 'homeboy/static-site-fixture-matrix-result/v1';
const WEBSITE_ARTIFACT_SCHEMA = 'blocks-engine/php-transformer/site-artifact/v1';

const DEFAULT_ENTRYPOINT = 'website/index.html';
const DEFAULT_FINDING_GROUPS = {
	button_style_loss: {
		patterns: [/default gray button/i, /button.*gray/i, /button.*style/i],
		candidate_repo: 'blocks-engine',
		repair_mode: 'transformer-style-parity',
	},
	broken_svg: {
		patterns: [/broken svg/i, /svg.*broken/i, /svg.*missing/i],
		candidate_repo: 'blocks-engine',
		repair_mode: 'svg-transformer-parity',
	},
	dropped_images: {
		patterns: [/dropped image/i, /missing image/i, /image.*missing/i, /asset.*missing/i],
		candidate_repo: 'static-site-importer',
		repair_mode: 'asset-materialization',
	},
	invalid_block_content: {
		patterns: [/unexpected or invalid content/i, /invalid block/i, /block validation/i],
		candidate_repo: 'blocks-engine',
		repair_mode: 'block-validation-parity',
	},
	runtime_target_gap: {
		patterns: [/runtime_dependency_target_missing/i, /html_canvas_runtime_fallback/i, /canvas/i, /animation/i, /script target/i],
		candidate_repo: 'blocks-engine',
		repair_mode: 'runtime-dom-target-parity',
	},
};

function discoverStaticSiteFixtures(root, options = {}) {
	const fixtureRoot = requiredDirectory(root || options.fixtureRoot || options.fixture_root, 'fixtureRoot');
	const entrypoint = options.entrypoint || 'index.html';
	const maxDepth = Number.isFinite(Number(options.maxDepth || options.max_depth)) ? Number(options.maxDepth || options.max_depth) : 2;
	const fixtures = [];

	visitFixtureDirectory(fixtureRoot, 0, maxDepth, (directory) => {
		const entryPath = path.join(directory, entrypoint);
		if (!fs.existsSync(entryPath) || !fs.statSync(entryPath).isFile()) {
			return;
		}

		fixtures.push(normalizeFixture({
			root: fixtureRoot,
			directory,
			entrypoint,
		}));
	});

	return fixtures.sort((left, right) => left.id.localeCompare(right.id));
}

function createStaticSiteFixtureMatrix(input = {}) {
	const fixtures = normalizeFixtures(input.fixtures || discoverStaticSiteFixtures(input.fixture_root || input.fixtureRoot, input));
	return {
		schema: FIXTURE_MATRIX_SCHEMA,
		id: input.id || input.run_id || input.runId || 'static-site-fixture-matrix',
		fixture_root: input.fixture_root || input.fixtureRoot || commonFixtureRoot(fixtures),
		entrypoint: input.entrypoint || 'index.html',
		count: fixtures.length,
		fixtures,
		artifacts: {
			result: input.result_artifact || input.resultArtifact || 'static-site-fixture-matrix-result.json',
			summary: input.summary_artifact || input.summaryArtifact || 'summary.json',
			findings: input.findings_artifact || input.findingsArtifact || 'finding-packets.json',
		},
	};
}

function buildStaticSiteFixtureArtifact(fixture, options = {}) {
	const normalized = normalizeFixture(fixture);
	const files = collectFixtureFiles(normalized.directory, options);
	const artifactFiles = files.map((file) => ({
		path: `website/${file.relative_path}`,
		source_path: file.absolute_path,
		type: file.type,
		bytes: file.bytes,
	}));

	return {
		schema: WEBSITE_ARTIFACT_SCHEMA,
		entrypoint: DEFAULT_ENTRYPOINT,
		entry_path: DEFAULT_ENTRYPOINT,
		files: artifactFiles,
		summary: {
			file_count: artifactFiles.length,
			entry_path: DEFAULT_ENTRYPOINT,
			has_css: artifactFiles.some((file) => file.path.endsWith('.css')),
			has_js: artifactFiles.some((file) => file.path.endsWith('.js')),
			has_images: artifactFiles.some((file) => isImagePath(file.path)),
		},
		source_metadata: {
			fixture_id: normalized.id,
			fixture_path: normalized.directory,
			fixture_entrypoint: normalized.entrypoint,
		},
	};
}

function buildStaticSiteFixtureMatrixRecipe(input = {}) {
	const matrix = input.matrix || createStaticSiteFixtureMatrix(input);
	const artifactsDirectory = input.artifactsDirectory || input.artifacts_directory || '/artifacts/static-site-fixture-matrix';
	const staticSiteImporter = normalizeStaticSiteImporterPlugin(input);
	const extraPlugins = staticSiteImporter
		? [staticSiteImporter.extraPlugin, ...normalizeArray(input.extraPlugins || input.extra_plugins)]
		: normalizeArray(input.extraPlugins || input.extra_plugins);
	const validationSteps = matrix.fixtures.map((fixture) => ({
		command: 'wordpress.wp-cli',
		args: [
			`command=static-site-importer validate-in-codebox --artifact=${shellToken(artifactPathForFixture(fixture, artifactsDirectory))} --slug=${shellToken(fixture.id)} --name=${shellToken(fixture.label)} --allow-missing-woocommerce`,
		],
		metadata: {
			fixture_id: fixture.id,
			artifact: artifactPathForFixture(fixture, artifactsDirectory),
			output: outputPathForFixture(fixture, artifactsDirectory),
		},
	}));
	return {
		schema: 'wp-codebox/workspace-recipe/v1',
		runtime: {
			wp: input.wordpressVersion || input.wordpress_version || 'latest',
			blueprint: input.blueprint || {},
		},
		inputs: {
			mounts: normalizeArray(input.mounts),
			...(extraPlugins.length > 0 ? { extraPlugins } : {}),
		},
		workflow: {
			steps: staticSiteImporter
				? [staticSiteImporter.activationStep, ...validationSteps]
				: validationSteps,
		},
		artifacts: {
			directory: artifactsDirectory,
		},
	};
}

function normalizeStaticSiteImporterPlugin(input = {}) {
	const source = input.staticSiteImporterPath || input.static_site_importer_path;
	if (typeof source !== 'string' || source.trim() === '') {
		return null;
	}

	const slugValue = input.staticSiteImporterSlug || input.static_site_importer_slug || path.basename(source);
	const pluginFile = input.staticSiteImporterPlugin || input.static_site_importer_plugin || `${slugValue}/${slugValue}.php`;
	return {
		extraPlugin: {
			source,
			slug: slugValue,
			activate: true,
		},
		activationStep: {
			command: 'wordpress.ensure-plugin-active',
			args: [`plugin=${pluginFile}`],
			metadata: {
				plugin: pluginFile,
				slug: slugValue,
			},
		},
	};
}

function normalizeStaticSiteFixtureMatrixResult(input = {}) {
	const matrix = input.matrix || createStaticSiteFixtureMatrix(input);
	const results = normalizeArray(input.results || input.fixture_results || input.fixtureResults).map((result) => normalizeFixtureResult(result));
	const resultByFixture = new Map(results.map((result) => [result.fixture_id, result]));
	const fixtureResults = matrix.fixtures.map((fixture) => resultByFixture.get(fixture.id) || normalizeFixtureResult({ fixture_id: fixture.id, status: 'not_run' }));
	const findings = fixtureResults.flatMap((result) => findingsForFixtureResult(result, { matrix }));
	const grouped = groupFindings(findings);

	return {
		schema: FIXTURE_MATRIX_RESULT_SCHEMA,
		matrix_id: matrix.id,
		fixture_root: matrix.fixture_root,
		summary: {
			fixture_count: matrix.fixtures.length,
			succeeded: fixtureResults.filter((result) => result.status === 'passed').length,
			failed: fixtureResults.filter((result) => result.status === 'failed').length,
			not_run: fixtureResults.filter((result) => result.status === 'not_run').length,
			finding_count: findings.length,
			groups: Object.fromEntries(Object.entries(grouped).map(([key, items]) => [key, items.length])),
		},
		fixtures: fixtureResults,
		findings,
		fanout_groups: Object.entries(grouped).map(([group_key, items], index) => ({
			key: group_key,
			index,
			findings: items,
		})),
	};
}

function collectStaticSiteFixtureMatrixRunResults(input = {}) {
	const matrix = input.matrix || createStaticSiteFixtureMatrix(input);
	const outputDirectory = requiredString(input.outputDirectory || input.output_directory, 'outputDirectory');
	const codeboxOutput = input.codeboxOutput || input.codebox_output || readJsonFileIfExists(input.outputFile || input.output_file) || null;
	const codeboxError = input.codeboxError || input.codebox_error || null;
	const runtimePayloads = collectRuntimePayloads(codeboxOutput);
	const results = matrix.fixtures.map((fixture) => {
		const fixtureArtifactsDirectory = path.join(outputDirectory, fixture.id);
		const payloads = [
			...runtimePayloads.filter((payload) => payloadMatchesFixture(payload, fixture)),
			...readFixturePayloadFiles(fixtureArtifactsDirectory),
		];
		return normalizeCollectedFixtureResult({
			fixture,
			payloads,
			fixtureArtifactsDirectory,
			codeboxError,
		});
	});

	return normalizeStaticSiteFixtureMatrixResult({ matrix, results });
}

function writeStaticSiteFixtureMatrixResultArtifacts(input = {}) {
	const outputDirectory = requiredString(input.outputDirectory || input.output_directory, 'outputDirectory');
	const matrix = input.matrix || createStaticSiteFixtureMatrix(input);
	const result = input.result || normalizeStaticSiteFixtureMatrixResult({ ...input, matrix });

	writeJsonFile(path.join(outputDirectory, 'static-site-fixture-matrix-result.json'), result);
	writeJsonFile(path.join(outputDirectory, 'summary.json'), result.summary);
	writeJsonFile(path.join(outputDirectory, 'finding-packets.json'), result.findings);

	return result;
}

function writeStaticSiteFixtureMatrixArtifacts(input = {}) {
	const outputDirectory = requiredString(input.outputDirectory || input.output_directory, 'outputDirectory');
	const matrix = input.matrix || createStaticSiteFixtureMatrix(input);
	const result = input.result || normalizeStaticSiteFixtureMatrixResult({ ...input, matrix });

	fs.mkdirSync(outputDirectory, { recursive: true });
	for (const fixture of matrix.fixtures) {
		const fixtureDirectory = path.join(outputDirectory, fixture.id);
		fs.mkdirSync(fixtureDirectory, { recursive: true });
		writeJsonFile(path.join(fixtureDirectory, 'artifact.json'), buildStaticSiteFixtureArtifact(fixture, input));
	}

	writeJsonFile(path.join(outputDirectory, 'matrix.json'), matrix);
	writeJsonFile(path.join(outputDirectory, 'static-site-fixture-matrix-result.json'), result);
	writeJsonFile(path.join(outputDirectory, 'summary.json'), result.summary);
	writeJsonFile(path.join(outputDirectory, 'finding-packets.json'), result.findings);

	return {
		matrix,
		result,
		artifact_refs: [
			artifactRef('matrix', path.join(outputDirectory, 'matrix.json'), 'matrix'),
			artifactRef('result', path.join(outputDirectory, 'static-site-fixture-matrix-result.json'), 'diagnostic'),
			artifactRef('summary', path.join(outputDirectory, 'summary.json'), 'summary'),
			artifactRef('finding-packets', path.join(outputDirectory, 'finding-packets.json'), 'diagnostic'),
		],
	};
}

function findingsForFixtureResult(result, context = {}) {
	const diagnostics = normalizeArray(result.diagnostics || result.findings || result.messages);
	const findings = diagnostics.map((diagnostic, index) => normalizeDiagnosticFinding(diagnostic, result, index));
	if (result.status === 'failed' && findings.length === 0) {
		findings.push(normalizeDiagnosticFinding({
			kind: 'fixture_failed',
			message: result.error || 'Static-site fixture validation failed without a structured diagnostic.',
		}, result, 0));
	}
	if (context.matrix?.fixtures?.some((fixture) => fixture.id === result.fixture_id) && result.status === 'not_run') {
		findings.push(normalizeDiagnosticFinding({
			kind: 'fixture_not_run',
			message: 'Static-site fixture was discovered but did not produce a validation result.',
		}, result, 0));
	}
	return findings;
}

function normalizeDiagnosticFinding(diagnostic, result, index) {
	const raw = diagnostic && typeof diagnostic === 'object' ? diagnostic : { message: String(diagnostic || '') };
	const message = raw.message || raw.reason || raw.detail || raw.code || result.error || '';
	const group = classifyStaticSiteFinding({ ...raw, message });
	const id = raw.id || `${result.fixture_id || 'fixture'}:${group.group_key}:${index + 1}`;

	return {
		id,
		kind: raw.kind || raw.code || 'static_site_fixture_diagnostic',
		category: raw.category || group.group_key,
		group_key: group.group_key,
		severity: raw.severity || (result.status === 'failed' ? 'error' : 'warning'),
		fixture_id: result.fixture_id || '',
		path: raw.path || raw.source_path || result.fixture_path || '',
		source_path: raw.source_path || raw.path || result.fixture_path || '',
		selector: raw.selector || '',
		reason: message,
		repair_mode: raw.repair_mode || group.repair_mode,
		candidate_repo: raw.candidate_repo || group.candidate_repo,
		artifact_refs: normalizeArray(raw.artifact_refs),
		raw,
	};
}

function classifyStaticSiteFinding(input = {}) {
	const haystack = [input.kind, input.code, input.category, input.message, input.reason, input.detail]
		.filter(Boolean)
		.join(' ');

	for (const [group_key, group] of Object.entries(DEFAULT_FINDING_GROUPS)) {
		if (group.patterns.some((pattern) => pattern.test(haystack))) {
			return { group_key, candidate_repo: group.candidate_repo, repair_mode: group.repair_mode };
		}
	}

	return {
		group_key: DEFAULT_FINDING_GROUPS[input.group_key] ? input.group_key : 'static_site_import_quality',
		candidate_repo: input.candidate_repo || 'static-site-importer',
		repair_mode: input.repair_mode || 'import-validation',
	};
}

function normalizeFixture(input) {
	const directory = requiredDirectory(input.directory || input.path || input.fixture_path || input.fixturePath, 'fixture.directory');
	const root = input.root || input.fixture_root || input.fixtureRoot || path.dirname(directory);
	const relative = path.relative(path.resolve(root), path.resolve(directory));
	const id = slug(input.id || input.slug || (relative && !relative.startsWith('..') ? relative : path.basename(directory)));
	return {
		id,
		label: input.label || input.name || id,
		directory,
		fixture_path: directory,
		fixture_root: root,
		entrypoint: input.entrypoint || 'index.html',
	};
}

function normalizeFixtures(fixtures) {
	return normalizeArray(fixtures).map((fixture) => normalizeFixture(fixture));
}

function normalizeFixtureResult(input) {
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
		ssi_validation: input.ssi_validation || input.ssiValidation || null,
		import_report: input.import_report || input.importReport || null,
		quality_metrics: input.quality_metrics || input.qualityMetrics || {},
		blocks_engine_diagnostics: normalizeArray(input.blocks_engine_diagnostics || input.blocksEngineDiagnostics),
		invalid_block_counts: input.invalid_block_counts || input.invalidBlockCounts || {},
		missing_assets: normalizeArray(input.missing_assets || input.missingAssets),
		runtime_target_gaps: normalizeArray(input.runtime_target_gaps || input.runtimeTargetGaps),
		diagnostics: normalizeArray(input.diagnostics || input.findings || input.messages),
		artifact_refs: normalizeArray(input.artifact_refs || input.artifactRefs),
		artifacts: input.artifacts || {},
		raw: input,
	};
}

function normalizeCollectedFixtureResult({ fixture, payloads, fixtureArtifactsDirectory, codeboxError }) {
	const merged = mergeObjects(payloads);
	const diagnostics = collectFixtureDiagnostics(merged);
	const artifactRefs = collectFixtureArtifactRefs(merged, fixtureArtifactsDirectory);
	const error = firstString([
		merged.error,
		merged.message && isFailurePayload(merged) ? merged.message : '',
		codeboxError && payloads.length === 0 ? codeboxError.message || String(codeboxError) : '',
	]);
	const success = inferFixtureSuccess(merged, diagnostics, error, payloads.length);
	const status = fixtureStatus(payloads.length, error, success);

	return normalizeFixtureResult({
		fixture_id: fixture.id,
		fixture_path: fixture.fixture_path,
		status,
		success,
		error,
		ssi_validation: merged.ssi_validation || merged.ssiValidation || merged.validation || merged.static_site_importer || null,
		import_report: merged.import_report || merged.importReport || merged.report || null,
		quality_metrics: collectQualityMetrics(merged),
		blocks_engine_diagnostics: collectBlocksEngineDiagnostics(merged),
		invalid_block_counts: collectInvalidBlockCounts(merged),
		missing_assets: collectMissingAssets(merged),
		runtime_target_gaps: collectRuntimeTargetGaps(merged),
		diagnostics,
		artifact_refs: artifactRefs,
		artifacts: merged.artifacts || {},
		raw: { payloads },
	});
}

function collectFixtureDiagnostics(payload) {
	const diagnostics = [
		...normalizeArray(payload.diagnostics),
		...normalizeArray(payload.findings),
		...normalizeArray(payload.messages),
		...normalizeArray(payload.errors),
		...normalizeArray(payload.warnings),
		...collectBlocksEngineDiagnostics(payload),
		...collectRuntimeTargetGaps(payload).map((gap) => ({ kind: 'runtime_target_gap', ...objectValue(gap), message: diagnosticMessage(gap) || 'Runtime target gap detected.' })),
		...collectMissingAssets(payload).map((asset) => ({ kind: missingAssetKind(asset), ...objectValue(asset), message: diagnosticMessage(asset) || 'Missing imported asset.' })),
	];
	const invalidBlockCount = Object.values(collectInvalidBlockCounts(payload)).reduce((sum, value) => sum + numberValue(value), 0);
	if (invalidBlockCount > 0) {
		diagnostics.push({
			kind: 'invalid_block_content',
			message: `${invalidBlockCount} invalid block${invalidBlockCount === 1 ? '' : 's'} reported by SSI validation.`,
		});
	}
	return diagnostics;
}

function collectQualityMetrics(payload) {
	return compactObject({
		...(payload.quality_metrics || payload.qualityMetrics || {}),
		...(payload.quality || {}),
		...(payload.import_report?.report?.quality || payload.importReport?.report?.quality || payload.report?.quality || {}),
	});
}

function collectInvalidBlockCounts(payload) {
	const quality = collectQualityMetrics(payload);
	return compactObject({
		invalid_block_count: payload.invalid_block_count || payload.invalidBlockCount || quality.invalid_block_count,
		invalid_blocks: payload.invalid_blocks || payload.invalidBlocks || quality.invalid_blocks,
		editor_invalid_blocks: payload.editor_invalid_blocks || payload.editorInvalidBlocks || quality.editor_invalid_blocks,
	});
}

function collectMissingAssets(payload) {
	return [
		...normalizeArray(payload.missing_assets || payload.missingAssets),
		...normalizeArray(payload.dropped_images || payload.droppedImages),
		...normalizeArray(payload.import_report?.missing_assets || payload.importReport?.missing_assets),
		...normalizeArray(payload.report?.missing_assets),
	];
}

function collectRuntimeTargetGaps(payload) {
	return [
		...normalizeArray(payload.runtime_target_gaps || payload.runtimeTargetGaps),
		...normalizeArray(payload.runtime_targets_missing || payload.runtimeTargetsMissing),
		...normalizeArray(payload.blocks_engine?.runtime_target_gaps || payload.blocksEngine?.runtimeTargetGaps),
	];
}

function collectBlocksEngineDiagnostics(payload) {
	return [
		...normalizeArray(payload.blocks_engine_diagnostics || payload.blocksEngineDiagnostics),
		...normalizeArray(payload.blocks_engine?.diagnostics || payload.blocksEngine?.diagnostics),
		...normalizeArray(payload.transformer_diagnostics || payload.transformerDiagnostics),
	];
}

function collectFixtureArtifactRefs(payload, fixtureArtifactsDirectory) {
	const refs = [
		...normalizeArray(payload.artifact_refs || payload.artifactRefs),
		...normalizeArray(payload.artifacts?.refs),
	];
	for (const [key, value] of Object.entries(payload.artifacts || {})) {
		if (value && typeof value === 'object' && !Array.isArray(value) && (value.path || value.file || value.href)) {
			refs.push({ artifact_id: key, kind: value.kind || key, ...value });
		} else if (typeof value === 'string') {
			refs.push({ artifact_id: key, kind: key, path: value });
		}
	}
	for (const fileName of ['artifact.json', 'validation-result.json', 'import-report.json']) {
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
	return ['status', 'success', 'ok', 'passed', 'error', 'diagnostics', 'findings', 'summary', 'artifacts', 'upstream_gaps', 'runtime_target_gaps', 'blocks_engine', 'import_report']
		.some((key) => Object.hasOwn(value, key));
}

function readFixturePayloadFiles(directory) {
	return ['validation-result.json', 'result.json', 'import-report.json', 'quality.json', 'blocks-engine-diagnostics.json']
		.map((fileName) => readJsonFileIfExists(path.join(directory, fileName)))
		.filter(Boolean);
}

function payloadMatchesFixture(payload, fixture) {
	return fixtureIdentity(payload) === fixture.id;
}

function fixtureIdentity(payload) {
	return payload?.fixture_id || payload?.fixtureId || payload?.fixture?.id || payload?.metadata?.fixture_id || payload?.metadata?.fixtureId || '';
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
	const maxFiles = Number.isFinite(Number(options.maxFiles || options.max_files)) ? Number(options.maxFiles || options.max_files) : 1000;
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
			files.push({
				relative_path: relativePath,
				absolute_path: entryPath,
				type: fileType(relativePath),
				bytes: stat.size,
			});
			if (files.length > maxFiles) {
				throw new Error(`Fixture ${directory} has more than ${maxFiles} files.`);
			}
		}
	};
	visit(directory);
	return files.sort((left, right) => left.relative_path.localeCompare(right.relative_path));
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

function outputPathForFixture(fixture, artifactsDirectory) {
	return path.join(artifactsDirectory, fixture.id, 'validation-result.json');
}

function groupFindings(findings) {
	return findings.reduce((groups, finding) => {
		const key = finding.group_key || 'static_site_import_quality';
		groups[key] = groups[key] || [];
		groups[key].push(finding);
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
	if (extension === '.svg') {
		return 'image/svg+xml';
	}
	if (extension === '.png') {
		return 'image/png';
	}
	if (extension === '.jpg' || extension === '.jpeg') {
		return 'image/jpeg';
	}
	if (extension === '.webp') {
		return 'image/webp';
	}
	return 'application/octet-stream';
}

function isImagePath(filePath) {
	return /\.(png|jpe?g|gif|webp|svg)$/i.test(filePath);
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
	return Object.fromEntries(Object.entries(value || {}).filter(([, item]) => item !== undefined && item !== null && item !== ''));
}

function objectValue(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function numberValue(value) {
	const number = Number(value);
	return Number.isFinite(number) ? number : 0;
}

function firstString(values) {
	return values.map((value) => String(value || '').trim()).find(Boolean) || '';
}

function diagnosticMessage(value) {
	if (typeof value === 'string') {
		return value;
	}
	return value?.message || value?.reason || value?.detail || value?.path || value?.target || value?.selector || '';
}

function missingAssetKind(value) {
	const message = diagnosticMessage(value);
	return /\.svg(?:\b|$)/i.test(message) ? 'broken_svg' : 'dropped_images';
}

function readJsonFileIfExists(filePath) {
	if (!filePath || !fs.existsSync(filePath) || !fs.statSync(filePath).isFile()) {
		return null;
	}
	try {
		return JSON.parse(fs.readFileSync(filePath, 'utf8'));
	} catch (error) {
		return {
			status: 'failed',
			error: `Unable to parse JSON artifact ${filePath}: ${error.message}`,
			artifact_refs: [artifactRef('unparseable-json', filePath, 'diagnostic')],
		};
	}
}

function parseJsonPayloadsFromText(text) {
	if (typeof text !== 'string' || !text.trim()) {
		return [];
	}
	const payloads = [];
	const candidates = new Set([text.trim(), ...text.split(/\r?\n/).map((line) => line.trim())]);
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
			// WP-CLI output may mix human text and JSON; non-JSON lines are ignored.
		}
	}
	return payloads;
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
	return {
		schema: 'homeboy/artifact-ref/v1',
		artifact_id,
		kind,
		path: filePath,
	};
}

function slug(value) {
	return String(value || 'fixture')
		.toLowerCase()
		.replace(/[^a-z0-9._-]+/g, '-')
		.replace(/^-+|-+$/g, '') || 'fixture';
}

function shellToken(value) {
	const text = String(value || '');
	return /^[A-Za-z0-9_./:@=-]+$/.test(text) ? text : `'${text.replace(/'/g, "'\\''")}'`;
}

module.exports = {
	FIXTURE_MATRIX_RESULT_SCHEMA,
	FIXTURE_MATRIX_SCHEMA,
	WEBSITE_ARTIFACT_SCHEMA,
	buildStaticSiteFixtureArtifact,
	buildStaticSiteFixtureMatrixRecipe,
	classifyStaticSiteFinding,
	collectStaticSiteFixtureMatrixRunResults,
	createStaticSiteFixtureMatrix,
	discoverStaticSiteFixtures,
	normalizeStaticSiteFixtureMatrixResult,
	writeStaticSiteFixtureMatrixResultArtifacts,
	writeStaticSiteFixtureMatrixArtifacts,
};
