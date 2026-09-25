'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
	HOMEBOY_FUZZ_CAMPAIGN_SCHEMA,
	WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA,
	dispatchWordPressFuzzRunnerResult,
	isLayoutSweepWorkloadDeclaration,
	layoutSweepDeclarationFromWorkload,
	writeHomeboyFuzzArtifactFiles,
	writeHomeboyFuzzResultsFile,
} = require('../lib/wordpress-fuzz-runner');
const {
	LAYOUT_SWEEP_COMMAND,
	LAYOUT_SWEEP_SUMMARY_SCHEMA,
	LAYOUT_SWEEP_WORKLOAD_SCHEMA,
} = require('../lib/wordpress-layout-sweep-workload');

// ---------------------------------------------------------------------------
// A layout-sweep workload declaration dispatches through `buildLayoutSweepRecipe`
// + the runner's existing Codebox client path (a stubbed `wp-codebox` binary
// stands in for a live browser run) and a non-layout-sweep workload keeps
// taking the existing `runWordPressFuzzRunnerResult` path untouched.
// ---------------------------------------------------------------------------

async function main() {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), 'wordpress-fuzz-runner-layout-sweep-'));
	try {
		await assertLayoutSweepWorkloadDispatchesThroughCodebox(root);
		await assertNonLayoutSweepWorkloadKeepsExistingPath(root);
		assertCoreEnvelopeCarriesTheDeclaration();
	} finally {
		fs.rmSync(root, { recursive: true, force: true });
	}
}

// Homeboy core loads workload files as `homeboy/fuzz-workload/v1` envelopes
// (id and safety_class required). The declaration rides at
// `workload.definition`, like other WordPress workloads.
function assertCoreEnvelopeCarriesTheDeclaration() {
	const declaration = {
		schema: LAYOUT_SWEEP_WORKLOAD_SCHEMA,
		preview: { url: '/canvas-demo/' },
		containerSelector: '.wp-block-tabor-canvas',
		itemSelector: ':scope > .canvas__grid > .canvas__item',
	};
	const envelope = {
		schema: 'homeboy/fuzz-workload/v1',
		id: 'canvas-preview',
		safety_class: 'read_only',
		workload: { definition: declaration },
	};
	assert.equal(isLayoutSweepWorkloadDeclaration(envelope), true);
	const found = layoutSweepDeclarationFromWorkload(envelope);
	assert.equal(found.schema, LAYOUT_SWEEP_WORKLOAD_SCHEMA);
	assert.equal(found.id, 'canvas-preview', 'the envelope id becomes the declaration id when the definition has none');
	assert.equal(layoutSweepDeclarationFromWorkload({ ...envelope, workload: { definition: { ...declaration, id: 'own-id' } } }).id, 'own-id');
	assert.equal(layoutSweepDeclarationFromWorkload({ schema: 'homeboy/fuzz-workload/v1', id: 'x', safety_class: 'read_only', workload: { runner: 'wp-codebox' } }), null);
	console.log('wordpress fuzz runner layout-sweep core envelope smoke passed');
}

async function assertLayoutSweepWorkloadDispatchesThroughCodebox(root) {
	const tempDir = path.join(root, 'layout-sweep');
	fs.mkdirSync(tempDir, { recursive: true });
	const workloadPath = path.join(tempDir, 'workload.json');
	const resultsPath = path.join(tempDir, 'campaign.json');
	const artifactRoot = path.join(tempDir, 'artifacts');
	const fakeCodeboxBin = path.join(tempDir, 'wp-codebox');

	const declaration = {
		schema: LAYOUT_SWEEP_WORKLOAD_SCHEMA,
		id: 'canvas-grid-dispatch',
		preview: { url: '/canvas-demo/' },
		containerSelector: '.wp-block-tabor-canvas',
		itemSelector: ':scope > .canvas__grid > .canvas__item',
		seed: 3,
		scenarios: ['sweep'],
	};
	const envelope = { schema: 'homeboy/fuzz-workload/v1', id: 'canvas-grid-dispatch', safety_class: 'read_only', workload: { definition: declaration } };
	fs.writeFileSync(workloadPath, `${JSON.stringify(envelope, null, 2)}\n`);
	assert.equal(isLayoutSweepWorkloadDeclaration(envelope), true);

	const fixtureSummary = {
		schema: LAYOUT_SWEEP_SUMMARY_SCHEMA,
		command: LAYOUT_SWEEP_COMMAND,
		status: 'failed',
		url: '/canvas-demo/',
		seed: 3,
		profile: 'quick',
		findings: [{
			kind: 'overflow',
			container: '#0 .wp-block-tabor-canvas',
			item: '#1 .canvas__item',
			scenarios: ['sweep'],
			count: 1,
			widthRange: [768, 900],
			worstMagnitude: 12,
			sample: { scenario: 'sweep', kind: 'overflow', container: '#0 .wp-block-tabor-canvas', item: '#1 .canvas__item', width: 800 },
			replay: { command: LAYOUT_SWEEP_COMMAND, args: ['width=800'] },
			suppressed: false,
		}],
		unsuppressedFindings: 1,
		replay: { command: LAYOUT_SWEEP_COMMAND, args: ['url=/canvas-demo/'] },
		perf: null,
	};
	writeFakeWpCodeboxBin(fakeCodeboxBin, fixtureSummary);

	const result = await dispatchWordPressFuzzRunnerResult({
		env: {
			workloadPath,
			runId: 'layout-sweep-run',
			workloadId: 'canvas-grid-dispatch',
			artifactRoot,
			wpCodeboxBin: fakeCodeboxBin,
		},
	});

	assert.equal(result.schema, WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA);
	assert.equal(result.run_id, 'layout-sweep-run');
	assert.equal(result.succeeded, false, 'an open (unsuppressed) mapped finding must not report success');
	assert.equal(result.homeboy_fuzz_campaign.schema, HOMEBOY_FUZZ_CAMPAIGN_SCHEMA);
	assert.equal(result.homeboy_fuzz_campaign.id, 'layout-sweep-run');
	assert.equal(result.homeboy_fuzz_campaign.findings.length, 1);

	const finding = result.homeboy_fuzz_campaign.findings[0];
	assert.equal(finding.kind, 'overflow');
	assert.equal(finding.status, 'open');
	assert.deepEqual(finding.identity, { kind: 'overflow', container: '#0 .wp-block-tabor-canvas', item: '#1 .canvas__item' });
	// The stable identity fingerprint is the field Homeboy fuzz findings use
	// for identity, so `homeboy fuzz compare` can match findings across runs.
	assert.equal(typeof finding.fingerprint, 'string');
	assert.ok(finding.fingerprint.length > 0);
	assert.equal(finding.id, finding.fingerprint);

	// The Homeboy fuzz campaign is written through the runner's existing
	// results and artifact writers, unchanged for this workload type.
	writeHomeboyFuzzResultsFile(resultsPath, result.homeboy_fuzz_campaign);
	writeHomeboyFuzzArtifactFiles(artifactRoot, result);
	const persistedCampaign = JSON.parse(fs.readFileSync(resultsPath, 'utf8'));
	assert.equal(persistedCampaign.schema, HOMEBOY_FUZZ_CAMPAIGN_SCHEMA);
	assert.equal(persistedCampaign.findings[0].fingerprint, finding.fingerprint);

	// The layout-sweep summary WP Codebox wrote stays on disk at the
	// documented artifact path; the runner reads it in place rather than
	// duplicating it through a new writer.
	const summaryPath = path.join(artifactRoot, 'files', 'browser', 'layout-sweep', 'summary.json');
	assert.ok(fs.existsSync(summaryPath), 'the fixture layout-sweep summary artifact must remain readable on disk');
}

async function assertNonLayoutSweepWorkloadKeepsExistingPath(root) {
	const tempDir = path.join(root, 'standard');
	fs.mkdirSync(tempDir, { recursive: true });
	const workloadPath = path.join(tempDir, 'workload.json');
	const workload = { id: 'standard-workload', plan: { id: 'standard-plan', targets: [] } };
	fs.writeFileSync(workloadPath, JSON.stringify(workload));
	assert.equal(isLayoutSweepWorkloadDeclaration(workload), false);

	const result = await dispatchWordPressFuzzRunnerResult({
		env: {
			workloadPath,
			runId: 'standard-run',
			workloadId: 'standard-workload',
			artifactRoot: tempDir,
		},
		// A stubbed Codebox suite runner stands in for the real WP Codebox
		// client, matching how the existing generic-path smokes inject it.
		runFuzzSuite: async () => ({
			schema: 'wp-codebox/fuzz-suite-result/v1',
			request_id: 'standard-run',
			status: 'succeeded',
			cases: [{ id: 'case-a', status: 'passed' }],
		}),
	});

	assert.equal(result.schema, WORDPRESS_FUZZ_RUNNER_RESULT_SCHEMA);
	assert.equal(result.succeeded, true);
	assert.equal(result.wp_codebox_result.result_schema, 'wp-codebox/fuzz-suite-result/v1');
	assert.equal(result.homeboy_fuzz_campaign.metadata.wp_codebox_result_schema, 'wp-codebox/fuzz-suite-result/v1');
	// The generic campaign shape (`cases`) is untouched — a non-layout-sweep
	// workload never gets the layout-sweep-specific `findings` mapping.
	assert.equal(result.homeboy_fuzz_campaign.findings, undefined);
	assert.equal(result.homeboy_fuzz_campaign.cases.length, 1);
}

function writeFakeWpCodeboxBin(binPath, summary) {
	fs.writeFileSync(binPath, `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');

if (process.argv.includes('--version')) {
	process.stdout.write('0.21.0');
	process.exit(0);
}
if (process.argv[2] === 'runtime' && process.argv[3] === 'descriptor' && process.argv.includes('--json')) {
	process.stdout.write(JSON.stringify({
		schema: 'wp-codebox/runtime-descriptor/v1',
		contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } },
	}));
	process.exit(0);
}
if (process.argv[2] === 'recipe-run') {
	const artifactsIndex = process.argv.indexOf('--artifacts');
	const artifactsDir = process.argv[artifactsIndex + 1];
	const summaryDir = path.join(artifactsDir, 'files', 'browser', 'layout-sweep');
	fs.mkdirSync(summaryDir, { recursive: true });
	fs.writeFileSync(path.join(summaryDir, 'summary.json'), ${JSON.stringify(JSON.stringify(summary))});
	process.stdout.write(JSON.stringify({ status: 'ok' }));
	process.exit(0);
}
process.stderr.write('unexpected wp-codebox invocation: ' + process.argv.slice(2).join(' '));
process.exit(1);
`);
	fs.chmodSync(binPath, 0o755);
}

main().then(() => console.log('wordpress fuzz runner layout-sweep dispatch smoke passed')).catch((error) => {
	console.error(error);
	process.exitCode = 1;
});
