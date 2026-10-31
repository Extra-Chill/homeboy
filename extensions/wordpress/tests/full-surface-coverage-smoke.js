'use strict';

const assert = require('node:assert/strict');

const {
	assertFullSurfaceCoverageArtifacts,
	buildFullSurfaceCoverageArtifact,
	buildFullSurfaceCoverageArtifactFromRefs,
	collectFullSurfaceCoverageArtifactRefs,
	formatFullSurfaceCoverageMarkdownReport,
	normalizeFullSurfaceCoverageArtifactRefs,
	normalizeFullSurfaceCoverageManifest,
	validateFullSurfaceCoverageArtifacts,
} = require('../lib/full-surface-coverage');

const artifact = buildFullSurfaceCoverageArtifact({
	rest: { totals: { routeCount: 10, caseCount: 20, coveredCount: 8, uncoveredCount: 2, budgetFindingCount: 1 } },
	ajax: { totals: { actionCount: 6, plannedCount: 2, skippedCount: 4 } },
	database: { totals: { tableCount: 12, rowCount: 100, columnCount: 80, indexCount: 20, totalBytes: 1024 } },
	serverRequests: { totals: { requests: 5, blocked: 1, hosts: 3 } },
	browserRequests: { totals: { requests: 40, responses: 38, failures: 2, hosts: 4, transferSizeBytes: 9000 } },
});

assert.equal(artifact.schema, 'homeboy/wordpress-full-surface-coverage/v1');
assert.equal(artifact.surfaces.rest.covered, 8);
assert.equal(artifact.surfaces.ajax.planned, 2);
assert.equal(artifact.surfaces.database.columns, 80);
assert.equal(artifact.surfaces.browserRequests.failures, 2);

const markdown = formatFullSurfaceCoverageMarkdownReport(artifact);
assert.match(markdown, /REST API \| 8\/10/);
assert.match(markdown, /AJAX actions \| 2\/6/);
assert.match(markdown, /Database \| 12 tables/);
assert.match(markdown, /Browser requests \| 40 requests/);

const manifest = normalizeFullSurfaceCoverageManifest({
	required_surfaces: ['rest_api', 'admin_ajax', 'db', 'browser_network'],
});
assert.deepEqual(manifest.requiredSurfaces, ['ajax', 'browserRequests', 'database', 'rest']);
assert.equal(manifest.surfaces.rest.required, true);
assert.equal(manifest.surfaces.ajax.required, true);
assert.equal(manifest.surfaces.database.required, true);

const refs = normalizeFullSurfaceCoverageArtifactRefs({
	artifact_refs: [
		{ kind: 'benchmark-route-matrix-summary', path: 'rest.json' },
		{ kind: 'wordpress-ajax-action-plan', path: 'ajax.json' },
		{ role: 'database', file: 'db.json' },
		{ capability: 'browser_network', path: 'browser.json' },
	],
});
assert.equal(refs.length, 4);
assert.equal(refs[0].schema, 'homeboy/artifact-ref/v1');
assert.equal(refs[2].path, 'db.json');
assert.deepEqual(refs[3].capabilities, ['browser_network']);

const codeboxRefs = normalizeFullSurfaceCoverageArtifactRefs({
	artifacts: {
		'benchmark-rest-request-case-summary': {
			path: 'files/benchmark-rest-request-case-summary.json',
		},
		'benchmark-db-inventory': {
			artifact_schema: 'homeboy/wordpress-db-inventory/v1',
			file_refs: [{ path: 'files/benchmark-db-inventory.json' }],
		},
		'external-http-guardrail': {
			metadata: { source_schema: 'homeboy/wordpress-external-http-guardrail/v1' },
		},
		'browser-request-coverage': {
			artifactSchema: 'homeboy/browser-request-coverage/v1',
		},
	},
});
assert.equal(codeboxRefs.length, 4);
assert.equal(codeboxRefs[0].role, 'benchmark-rest-request-case-summary');
assert.equal(codeboxRefs[1].artifact_schema, 'homeboy/wordpress-db-inventory/v1');
assert.equal(codeboxRefs[1].file_refs[0].path, 'files/benchmark-db-inventory.json');

const codeboxValidation = validateFullSurfaceCoverageArtifacts({
	required_surfaces: ['rest', 'database', 'serverRequests', 'browserRequests'],
	artifact_refs: codeboxRefs,
});
assert.equal(codeboxValidation.status, 'passed');
assert.equal(codeboxValidation.producedSurfaceCount, 4);

const semanticKeyValidation = validateFullSurfaceCoverageArtifacts({
	requiredSurfaces: ['browserRequests'],
	artifactRefs: [
		{ semantic_key: 'browser.request.coverage', path: 'files/request-coverage.json' },
	],
});
assert.equal(semanticKeyValidation.status, 'passed');
assert.equal(semanticKeyValidation.surfaces.browserRequests.artifactRefs[0].semantic_key, 'browser.request.coverage');

const codeboxManifestRefs = normalizeFullSurfaceCoverageArtifactRefs({
	artifacts: {
		directory: '/tmp/wp-codebox-artifacts',
		files: [
			{ name: 'benchmark-route-matrix-summary', pathname: 'files/benchmark-route-matrix-summary.json' },
			{ name: 'benchmark-db-inventory', schema: 'homeboy/wordpress-db-inventory/v1', relative_path: 'files/db.json' },
		],
	},
});
assert.equal(codeboxManifestRefs.length, 2);
assert.equal(codeboxManifestRefs[0].path, 'files/benchmark-route-matrix-summary.json');
assert.equal(codeboxManifestRefs[1].source_schema, 'homeboy/wordpress-db-inventory/v1');

const codeboxManifestValidation = validateFullSurfaceCoverageArtifacts({
	requiredSurfaces: ['rest', 'database'],
	artifactRefs: codeboxManifestRefs,
});
assert.equal(codeboxManifestValidation.status, 'passed');

const restDbProfileValidation = validateFullSurfaceCoverageArtifacts({
	requiredSurfaces: ['database'],
	artifactRefs: [
		{ artifact_schema: 'homeboy/wordpress-rest-db-query-profile/v1', path: 'files/rest-db-query-profile.json' },
	],
});
assert.equal(restDbProfileValidation.status, 'passed');

const pathOnlySubstringValidation = validateFullSurfaceCoverageArtifacts({
	requiredSurfaces: ['serverRequests', 'browserRequests'],
	artifactRefs: [
		{ path: 'files/not-external-http-guardrail.json' },
		{ name: 'browser-request-coverage-not-an-artifact', path: 'files/browser-request-coverage-not-an-artifact.json' },
	],
});
assert.equal(pathOnlySubstringValidation.status, 'failed');
assert.equal(pathOnlySubstringValidation.producedSurfaceCount, 0);
assert.deepEqual(pathOnlySubstringValidation.missingSurfaces, ['browserRequests', 'serverRequests']);
assert.equal(pathOnlySubstringValidation.surfaces.serverRequests.diagnosticFallbackRefs.length, 1);
assert.equal(pathOnlySubstringValidation.surfaces.browserRequests.diagnosticFallbackRefs.length, 1);

const validation = validateFullSurfaceCoverageArtifacts({
	requiredSurfaces: ['rest', 'database', 'serverRequests', 'browserRequests'],
	artifactRefs: refs,
});
assert.equal(validation.status, 'failed');
assert.equal(validation.requiredSurfaceCount, 4);
assert.equal(validation.producedSurfaceCount, 3);
assert.equal(validation.missingSurfaceCount, 1);
assert.deepEqual(validation.missingSurfaces, ['serverRequests']);
assert.equal(validation.surfaces.rest.produced, true);
assert.equal(validation.surfaces.serverRequests.produced, false);

const artifactWithValidation = buildFullSurfaceCoverageArtifact({
	rest: { totals: { routeCount: 2, caseCount: 2, coveredCount: 2 } },
	requiredSurfaces: ['rest', 'database'],
	artifactRefs: [
		{ kind: 'benchmark-route-matrix-summary', path: 'rest.json' },
	],
});
assert.equal(artifactWithValidation.validation.status, 'failed');
assert.deepEqual(artifactWithValidation.validation.missingSurfaces, ['database']);
assert.equal(artifactWithValidation.artifactRefs.length, 1);

const markdownWithValidation = formatFullSurfaceCoverageMarkdownReport(artifactWithValidation);
assert.match(markdownWithValidation, /Required surface validation/);
assert.match(markdownWithValidation, /Status: failed/);
assert.match(markdownWithValidation, /Missing required surfaces: database/);

const codeboxBenchmarkArtifacts = {
	schema: 'wp-codebox/benchmark-artifacts/v1',
	artifactBundle: {
		id: 'bundle-one',
		directory: '/tmp/wp-codebox-artifacts',
		contentDigest: 'digest',
	},
	results: [{
		schema: 'wp-codebox/bench-results/v1',
		component_id: 'generic-component',
		scenarios: [{
			id: 'route-matrix',
			artifactRefs: [
				{ path: 'files/bench/generic-component/route-matrix-route-matrix-summary.json', kind: 'benchmark-route-matrix-summary', source: 'scenario-artifact' },
				{ path: 'files/bench/generic-component/route-matrix-db-inventory.json', kind: 'benchmark-db-inventory', source: 'scenario-artifact' },
			],
		}],
	}],
	scenarios: [{
		componentId: 'generic-component',
		scenarioId: 'route-matrix',
		artifactRefs: [
			{ path: 'files/browser/request-coverage.json', kind: 'browser-request-coverage', source: 'browser-artifact' },
		],
	}],
};
const codeboxArtifactManifest = {
	id: 'bundle-one',
	files: [
		{ path: 'manifest.json', kind: 'manifest', contentType: 'application/json' },
		{ path: 'files/server/external-http-guardrail.json', kind: 'external-http-guardrail', contentType: 'application/json' },
	],
};

const aggregatedRefs = collectFullSurfaceCoverageArtifactRefs({
	benchmarkArtifacts: codeboxBenchmarkArtifacts,
	manifest: codeboxArtifactManifest,
});
assert.equal(aggregatedRefs.length, 5);
assert.equal(aggregatedRefs.some((ref) => ref.kind === 'benchmark-route-matrix-summary'), true);
assert.equal(aggregatedRefs.some((ref) => ref.kind === 'external-http-guardrail'), true);

const aggregatedValidation = assertFullSurfaceCoverageArtifacts({
	requiredSurfaces: ['rest', 'database', 'serverRequests', 'browserRequests'],
	benchmarkArtifacts: codeboxBenchmarkArtifacts,
	manifest: codeboxArtifactManifest,
});
assert.equal(aggregatedValidation.status, 'passed');
assert.equal(aggregatedValidation.requiredSurfaceCount, 4);

const aggregatedArtifact = buildFullSurfaceCoverageArtifactFromRefs({
	requiredSurfaces: ['rest', 'database', 'serverRequests', 'browserRequests'],
	benchmarkArtifacts: codeboxBenchmarkArtifacts,
	manifest: codeboxArtifactManifest,
	failOnMissing: true,
});
assert.equal(aggregatedArtifact.validation.status, 'passed');
assert.equal(aggregatedArtifact.artifactRefs.length, 5);

assert.throws(() => buildFullSurfaceCoverageArtifactFromRefs({
	requiredSurfaces: ['rest', 'database', 'serverRequests', 'browserRequests'],
	benchmarkArtifacts: codeboxBenchmarkArtifacts,
	manifest: { id: 'bundle-without-server-surface', files: [{ path: 'manifest.json', kind: 'manifest', contentType: 'application/json' }] },
	failOnMissing: true,
}), /Missing required full-surface coverage artifacts: serverRequests/);

console.log('Full-surface coverage smoke passed.');
