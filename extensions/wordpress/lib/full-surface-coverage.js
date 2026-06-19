'use strict';

function isObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value);
}

function numberValue(value, fallback = 0) {
	const number = Number(value);
	return Number.isFinite(number) ? number : fallback;
}

const SURFACE_ALIASES = {
	rest: ['rest', 'rest_api', 'rest-api', 'route_matrix', 'route-matrix', 'rest-route-matrix', 'wordpress-rest-route-matrix', 'benchmark-route-matrix-summary', 'benchmark-rest-request-case-summary'],
	database: ['database', 'db', 'db_inventory', 'db-inventory', 'wordpress-db-inventory', 'benchmark-db-inventory', 'rest-db-query-profile', 'wordpress-rest-db-query-profile'],
	serverRequests: ['server_requests', 'server-requests', 'serverRequests', 'external_http', 'external-http', 'external-http-guardrail', 'wordpress-external-http-guardrail'],
	browserRequests: ['browser_requests', 'browser-requests', 'browserRequests', 'browser_network', 'browser-network', 'browser-request-coverage', 'browser-request-coverage-artifact'],
};

const SURFACE_LABELS = {
	rest: 'REST API',
	database: 'Database',
	serverRequests: 'Server requests',
	browserRequests: 'Browser requests',
};

function canonicalSurface(value) {
	const normalized = String(value || '').trim();
	if (!normalized) {
		return '';
	}
	for (const [surface, aliases] of Object.entries(SURFACE_ALIASES)) {
		if (aliases.includes(normalized)) {
			return surface;
		}
	}
	throw new TypeError(`Unknown full-surface coverage surface: ${normalized}`);
}

function maybeCanonicalSurface(value) {
	try {
		return canonicalSurface(value);
	} catch {
		return '';
	}
}

function normalizeFullSurfaceCoverageManifest(input = {}) {
	const manifestInput = hasFullSurfaceRequirements(input) || !isObject(input.manifest) ? input : input.manifest;
	let requiredSurfaces = [];
	if (Array.isArray(manifestInput.requiredSurfaces)) {
		requiredSurfaces = manifestInput.requiredSurfaces;
	} else if (Array.isArray(manifestInput.required_surfaces)) {
		requiredSurfaces = manifestInput.required_surfaces;
	} else if (Array.isArray(manifestInput.surfaces)) {
		requiredSurfaces = manifestInput.surfaces;
	} else if (isObject(manifestInput.surfaces)) {
		requiredSurfaces = Object.entries(manifestInput.surfaces)
			.filter(([, config]) => !isObject(config) || config.required !== false)
			.map(([surface]) => surface);
	}
	const canonicalRequired = [...new Set(requiredSurfaces.map(canonicalSurface))].sort();
	return {
		schema: 'homeboy/wordpress-full-surface-coverage-manifest/v1',
		type: 'wordpress-full-surface-coverage-manifest',
		requiredSurfaces: canonicalRequired,
		surfaces: Object.fromEntries(canonicalRequired.map((surface) => [surface, {
			required: true,
			aliases: SURFACE_ALIASES[surface],
		}])),
	};
}

function normalizeFullSurfaceCoverageArtifactRefs(input = {}) {
	const rawRefs = artifactRefInputs(input);
	const seen = new Set();
	const refs = [];
	for (const rawRef of rawRefs) {
		if (!isObject(rawRef)) {
			continue;
		}
		const capabilities = Array.isArray(rawRef.capabilities)
			? rawRef.capabilities.map(String)
			: rawRef.capability ? [String(rawRef.capability)] : [];
		const ref = {
			schema: 'homeboy/artifact-ref/v1',
			artifact_id: String(rawRef.artifact_id || rawRef.artifactId || rawRef.id || rawRef.path || rawRef.file || rawRef.directory || rawRef.url || rawRef.href || ''),
			name: String(rawRef.name || rawRef.label || ''),
			kind: String(rawRef.kind || rawRef.type || ''),
			role: String(rawRef.role || rawRef.artifact_role || rawRef.artifactRole || ''),
			artifact_schema: String(rawRef.artifact_schema || rawRef.artifactSchema || rawRef.content_schema || rawRef.contentSchema || rawRef.source_schema || ''),
			source_schema: String(rawRef.schema && rawRef.schema !== 'homeboy/artifact-ref/v1' ? rawRef.schema : ''),
			path: String(rawRef.path || rawRef.pathname || rawRef.file || rawRef.directory || rawRef.relativePath || rawRef.relative_path || ''),
			url: String(rawRef.url || rawRef.href || ''),
			capabilities,
			file_refs: Array.isArray(rawRef.file_refs) ? rawRef.file_refs : Array.isArray(rawRef.fileRefs) ? rawRef.fileRefs : [],
			metadata: isObject(rawRef.metadata) ? rawRef.metadata : {},
		};
		const key = `${ref.kind}:${ref.role}:${ref.artifact_id || ref.name || ref.path || ref.url || ref.artifact_schema}`;
		if (seen.has(key)) {
			continue;
		}
		seen.add(key);
		refs.push(ref);
	}
	return refs;
}

function collectFullSurfaceCoverageArtifactRefs(input = {}) {
	return normalizeFullSurfaceCoverageArtifactRefs(collectArtifactRefInputs(input));
}

function collectArtifactRefInputs(input) {
	if (Array.isArray(input)) {
		return input;
	}
	if (!isObject(input)) {
		return [];
	}
	const refs = [];
	appendArtifactRefInputs(refs, input.artifactRefs);
	appendArtifactRefInputs(refs, input.artifact_refs);
	appendArtifactRefInputs(refs, input.artifacts);
	appendArtifactRefInputs(refs, input.manifest);
	appendArtifactRefInputs(refs, input.benchmarkArtifacts || input.benchmark_artifacts);
	appendArtifactRefInputs(refs, input.benchResults || input.bench_results);
	appendArtifactRefInputs(refs, input.results);
	appendArtifactRefInputs(refs, input.scenarios);
	appendArtifactRefInputs(refs, artifactRefInputs(input));
	return refs;
}

function appendArtifactRefInputs(refs, value) {
	if (Array.isArray(value)) {
		for (const item of value) {
			appendArtifactRefInputs(refs, item);
		}
		return;
	}
	if (!isObject(value)) {
		return;
	}
	if (isArtifactRefLike(value)) {
		refs.push(value);
	}
	if (isObject(value.artifacts) && !Array.isArray(value.artifacts)) {
		refs.push(...artifactRefInputs({ artifacts: value.artifacts }));
	}
	if (Array.isArray(value.artifactRefs) || Array.isArray(value.artifact_refs)) {
		refs.push(...artifactRefInputs(value));
	}
	const manifestFiles = artifactManifestFiles(value);
	if (manifestFiles.length > 0) {
		refs.push(...manifestFiles);
	}
	if (Array.isArray(value.scenarios)) {
		appendArtifactRefInputs(refs, value.scenarios);
	}
	if (Array.isArray(value.results)) {
		appendArtifactRefInputs(refs, value.results);
	}
	if (value.schema === 'wp-codebox/benchmark-artifacts/v1' || value.schema === 'wp-codebox/bench-results/v1') {
		refs.push(...artifactRefInputs(value));
	}
}

function isArtifactRefLike(value) {
	return isObject(value) && Boolean(
		value.kind
		|| value.type
		|| value.role
		|| value.artifact_role
		|| value.artifactRole
		|| value.artifact_schema
		|| value.artifactSchema
		|| value.content_schema
		|| value.contentSchema
		|| value.source_schema
		|| value.path
		|| value.pathname
		|| value.file
		|| value.directory
		|| value.relativePath
		|| value.relative_path
		|| value.url
		|| value.href
	);
}

function artifactRefInputs(input) {
	if (Array.isArray(input)) {
		return input;
	}
	if (!isObject(input)) {
		return [];
	}
	for (const key of ['artifactRefs', 'artifact_refs', 'artifacts']) {
		if (Array.isArray(input[key])) {
			return input[key];
		}
		if (isObject(input[key])) {
			const files = artifactManifestFiles(input[key]);
			if (files.length > 0) {
				return files;
			}
			return Object.entries(input[key]).map(([name, value]) => (
				isObject(value) ? { role: name, name, ...value } : { role: name, name, path: value }
			));
		}
	}
	const files = artifactManifestFiles(input.manifest?.artifacts || input.manifest);
	if (files.length > 0) {
		return files;
	}
	return Object.entries(input)
		.filter(([key, value]) => SURFACE_ALIASES[maybeCanonicalSurface(key)] && isObject(value))
		.map(([key, value]) => ({ role: key, ...value }));
}

function artifactManifestFiles(value) {
	if (!isObject(value)) {
		return [];
	}
	const files = value.files || value.artifactFiles || value.artifact_files;
	if (Array.isArray(files)) {
		return files;
	}
	if (isObject(files)) {
		return Object.entries(files).map(([name, file]) => (
			isObject(file) ? { role: name, name, ...file } : { role: name, name, path: file }
		));
	}
	return [];
}

function validateFullSurfaceCoverageArtifacts(input = {}) {
	const manifest = normalizeFullSurfaceCoverageManifest(hasFullSurfaceRequirements(input) ? input : input.manifest ? input.manifest : input);
	const artifactRefs = collectFullSurfaceCoverageArtifactRefs(input.artifactRefs || input.artifact_refs || input.artifacts || input.benchmarkArtifacts || input.benchmark_artifacts || input.benchResults || input.bench_results || input.results || input.scenarios ? input : input.artifactRefs || []);
	const surfaces = Object.fromEntries(manifest.requiredSurfaces.map((surface) => {
		const matchingRefs = artifactRefs.filter((ref) => artifactRefMatchesSurface(ref, surface));
		return [surface, {
			required: true,
			produced: matchingRefs.length > 0,
			artifactRefs: matchingRefs,
		}];
	}));
	const missingSurfaces = Object.entries(surfaces)
		.filter(([, surface]) => !surface.produced)
		.map(([surface]) => surface);
	return {
		schema: 'homeboy/wordpress-full-surface-coverage-validation/v1',
		type: 'wordpress-full-surface-coverage-validation',
		status: missingSurfaces.length === 0 ? 'passed' : 'failed',
		requiredSurfaceCount: manifest.requiredSurfaces.length,
		producedSurfaceCount: Object.values(surfaces).filter((surface) => surface.produced).length,
		missingSurfaceCount: missingSurfaces.length,
		surfaces,
		missingSurfaces,
		artifactRefs,
	};
}

function hasFullSurfaceRequirements(input) {
	return isObject(input) && (
		Array.isArray(input.requiredSurfaces)
		|| Array.isArray(input.required_surfaces)
		|| Array.isArray(input.surfaces)
		|| (isObject(input.surfaces) && Object.keys(input.surfaces).length > 0)
	);
}

function assertFullSurfaceCoverageArtifacts(input = {}) {
	const validation = validateFullSurfaceCoverageArtifacts(input);
	if (validation.status !== 'passed') {
		throw new Error(`Missing required full-surface coverage artifacts: ${validation.missingSurfaces.join(', ')}`);
	}
	return validation;
}

function buildFullSurfaceCoverageArtifactFromRefs(input = {}) {
	const artifactRefs = collectFullSurfaceCoverageArtifactRefs(input);
	const artifact = buildFullSurfaceCoverageArtifact({
		...input,
		artifactRefs,
	});
	if (input.failOnMissing === true || input.fail_on_missing === true) {
		assertFullSurfaceCoverageArtifacts({
			...input,
			artifactRefs,
		});
	}
	return artifact;
}

function artifactRefMatchesSurface(ref, surface) {
	const aliases = SURFACE_ALIASES[surface].map(normalizeMatchValue);
	const values = artifactRefMatchValues(ref).map(normalizeMatchValue);
	return values.some((value) => aliases.some((alias) => value.includes(alias)));
}

function artifactRefMatchValues(ref) {
	return [
		ref.kind,
		ref.role,
		ref.name,
		ref.schema,
		ref.artifact_schema,
		ref.source_schema,
		ref.artifact_id,
		ref.path,
		ref.url,
		...ref.capabilities,
		...artifactRefFileValues(ref.file_refs),
		...artifactRefMetadataValues(ref.metadata),
	].filter(Boolean).map(String);
}

function artifactRefFileValues(fileRefs) {
	return (Array.isArray(fileRefs) ? fileRefs : []).flatMap((fileRef) => {
		if (!isObject(fileRef)) {
			return [fileRef];
		}
		return [fileRef.name, fileRef.path, fileRef.file, fileRef.href, fileRef.url, fileRef.mime];
	});
}

function artifactRefMetadataValues(metadata) {
	if (!isObject(metadata)) {
		return [];
	}
	return [
		metadata.name,
		metadata.kind,
		metadata.type,
		metadata.role,
		metadata.artifact_schema,
		metadata.artifactSchema,
		metadata.source_schema,
		metadata.sourceSchema,
		metadata.wp_codebox?.id,
		metadata.wp_codebox?.kind,
		metadata.wp_codebox?.name,
	];
}

function normalizeMatchValue(value) {
	return String(value || '').trim().toLowerCase().replace(/_/g, '-');
}

function buildFullSurfaceCoverageArtifact(input = {}) {
	const rest = isObject(input.rest) ? input.rest : {};
	const database = isObject(input.database) ? input.database : {};
	const serverRequests = isObject(input.serverRequests) ? input.serverRequests : {};
	const browserRequests = isObject(input.browserRequests) ? input.browserRequests : {};
	const artifact = {
		schema: 'homeboy/wordpress-full-surface-coverage/v1',
		type: 'wordpress-full-surface-coverage',
		surfaces: {
			rest: {
				routes: numberValue(rest.totals?.routeCount),
				cases: numberValue(rest.totals?.caseCount),
				covered: numberValue(rest.totals?.coveredCount),
				uncovered: numberValue(rest.totals?.uncoveredCount),
				budgetFindings: numberValue(rest.totals?.budgetFindingCount),
			},
			database: {
				tables: numberValue(database.totals?.tableCount),
				rows: numberValue(database.totals?.rowCount),
				columns: numberValue(database.totals?.columnCount),
				indexes: numberValue(database.totals?.indexCount),
				bytes: numberValue(database.totals?.totalBytes),
			},
			serverRequests: {
				requests: numberValue(serverRequests.totals?.requests),
				blocked: numberValue(serverRequests.totals?.blocked),
				hosts: numberValue(serverRequests.totals?.hosts),
			},
			browserRequests: {
				requests: numberValue(browserRequests.totals?.requests),
				responses: numberValue(browserRequests.totals?.responses),
				failures: numberValue(browserRequests.totals?.failures),
				hosts: numberValue(browserRequests.totals?.hosts),
				transferSizeBytes: numberValue(browserRequests.totals?.transferSizeBytes),
			},
		},
	};
	const artifactRefs = normalizeFullSurfaceCoverageArtifactRefs(input);
	if (input.manifest || input.requiredSurfaces || input.required_surfaces || artifactRefs.length > 0) {
		artifact.artifactRefs = artifactRefs;
		artifact.validation = validateFullSurfaceCoverageArtifacts({
			manifest: input.manifest || input,
			artifactRefs,
		});
	}
	return artifact;
}

function formatFullSurfaceCoverageMarkdownReport(input = {}, options = {}) {
	const artifact = input?.schema === 'homeboy/wordpress-full-surface-coverage/v1'
		? input
		: buildFullSurfaceCoverageArtifact(input);
	const rows = [
		['REST API', `${artifact.surfaces.rest.covered}/${artifact.surfaces.rest.routes}`, `cases ${artifact.surfaces.rest.cases}; budget findings ${artifact.surfaces.rest.budgetFindings}`],
		['Database', `${artifact.surfaces.database.tables} tables`, `${artifact.surfaces.database.rows} rows; ${artifact.surfaces.database.columns} columns; ${artifact.surfaces.database.indexes} indexes`],
		['Server requests', `${artifact.surfaces.serverRequests.requests} requests`, `${artifact.surfaces.serverRequests.blocked} blocked; ${artifact.surfaces.serverRequests.hosts} hosts`],
		['Browser requests', `${artifact.surfaces.browserRequests.requests} requests`, `${artifact.surfaces.browserRequests.failures} failures; ${artifact.surfaces.browserRequests.hosts} hosts`],
	];
	const report = [
		`## ${options.title || 'WordPress full-surface coverage'}`,
		'',
		'| Surface | Coverage | Detail |',
		'| --- | ---: | --- |',
		...rows.map((row) => `| ${row[0]} | ${row[1]} | ${row[2]} |`),
	];
	if (artifact.validation) {
		report.push(
			'',
			'## Required surface validation',
			'',
			`Status: ${artifact.validation.status}`,
			'',
			'| Surface | Required | Produced | Artifacts |',
			'| --- | ---: | ---: | ---: |',
			...Object.entries(artifact.validation.surfaces).map(([surface, result]) => `| ${SURFACE_LABELS[surface] || surface} | yes | ${result.produced ? 'yes' : 'no'} | ${result.artifactRefs.length} |`),
		);
		if (artifact.validation.missingSurfaces.length > 0) {
			report.push('', `Missing required surfaces: ${artifact.validation.missingSurfaces.join(', ')}`);
		}
	}
	return report.join('\n');
}

module.exports = {
	assertFullSurfaceCoverageArtifacts,
	buildFullSurfaceCoverageArtifact,
	buildFullSurfaceCoverageArtifactFromRefs,
	collectFullSurfaceCoverageArtifactRefs,
	formatFullSurfaceCoverageMarkdownReport,
	normalizeFullSurfaceCoverageArtifactRefs,
	normalizeFullSurfaceCoverageManifest,
	validateFullSurfaceCoverageArtifacts,
};
