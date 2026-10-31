'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	LAYOUT_SWEEP_COMMAND,
	LAYOUT_SWEEP_SUMMARY_SCHEMA,
	LAYOUT_SWEEP_FINDING_SCHEMA,
	LAYOUT_SWEEP_OBSERVATION_SCHEMA,
	normalizeLayoutSweepWorkloadDeclaration,
	buildLayoutSweepCodeboxArgs,
	buildLayoutSweepRecipeStep,
	buildLayoutSweepRecipe,
	layoutSweepFindingFingerprint,
	mapLayoutSweepSummaryToFuzzFindings,
	mapLayoutSweepPerfToObservations,
	mapLayoutSweepSummaryToWorkloadResult,
	buildLayoutSweepFuzzCampaign,
} = require('../lib/wordpress-layout-sweep-workload');

// ---------------------------------------------------------------------------
// Fixture: a minimal consumer declaration (a Canvas-style block), and two
// fixture `wp-codebox/layout-sweep/v1` summaries for the same declaration —
// a baseline and a candidate — with one finding resolved, one new, one
// unchanged, one suppressed (accepted), one error finding, and a drag perf
// block. No live browser or WP Codebox process is involved.
// ---------------------------------------------------------------------------

const declaration = {
	id: 'canvas-grid',
	preview: { url: '/canvas-demo/' },
	containerSelector: '.wp-block-tabor-canvas',
	itemSelector: ':scope > .canvas__grid > .canvas__item',
	minWidth: 360,
	maxWidth: 1400,
	profile: 'quick',
	seed: 7,
	concurrency: 2,
	scenarios: ['sweep', 'history'],
	accepted: [{ kind: 'tiny-text', container: '#0 .wp-block-tabor-canvas', item: null }],
	timeout: '120s',
};

function findingGroup(overrides = {}) {
	return {
		schema: 'wp-codebox/layout-sweep-finding/v1',
		identity: { kind: overrides.kind, container: overrides.container ?? null, item: overrides.item ?? null },
		kind: overrides.kind,
		container: overrides.container ?? null,
		item: overrides.item ?? null,
		scenarios: overrides.scenarios || ['sweep'],
		count: overrides.count ?? 1,
		widthRange: overrides.widthRange ?? [768, 900],
		worstMagnitude: overrides.worstMagnitude ?? 12,
		sample: overrides.sample ?? { scenario: 'sweep', kind: overrides.kind, container: overrides.container ?? null, item: overrides.item ?? null, width: 800, by: overrides.worstMagnitude ?? 12 },
		replay: { command: LAYOUT_SWEEP_COMMAND, args: overrides.replayArgs || ['width=800'] },
		suppressed: overrides.suppressed === true,
	};
}

function baseSummary(findings, extra = {}) {
	return {
		schema: LAYOUT_SWEEP_SUMMARY_SCHEMA,
		command: LAYOUT_SWEEP_COMMAND,
		status: findings.some((finding) => !finding.suppressed) ? 'failed' : 'passed',
		url: '/canvas-demo/',
		seed: 7,
		profile: 'quick',
		range: { min: 360, max: 1400, step: 16, height: 900 },
		scenarios: ['sweep', 'history'],
		containers: 1,
		items: 4,
		resizes: 12,
		findings,
		unsuppressedFindings: findings.filter((finding) => !finding.suppressed).length,
		replay: { command: LAYOUT_SWEEP_COMMAND, args: ['url=/canvas-demo/', 'container-selector=.wp-block-tabor-canvas', 'seed=7'] },
		missingResources: [],
		perf: null,
		...extra,
	};
}

const unchangedFinding = findingGroup({ kind: 'overflow', container: '#0 .wp-block-tabor-canvas', item: '#1 .canvas__item', worstMagnitude: 9 });
const resolvedInBaselineFinding = findingGroup({ kind: 'leak', container: '#0 .wp-block-tabor-canvas', item: '#2 .canvas__item', worstMagnitude: 22 });
const newInCandidateFinding = findingGroup({ kind: 'overlap', container: '#0 .wp-block-tabor-canvas', item: '#0 .canvas__item × #1 .canvas__item', worstMagnitude: 0 });
const suppressedFinding = findingGroup({ kind: 'tiny-text', container: '#0 .wp-block-tabor-canvas', item: null, worstMagnitude: 8, suppressed: true });
const errorFinding = findingGroup({ kind: 'error', container: null, item: null, worstMagnitude: 0, sample: { scenario: 'sweep', kind: 'error', container: null, item: null } });

const baselineSummary = baseSummary(
	[unchangedFinding, resolvedInBaselineFinding, suppressedFinding, errorFinding],
	{
		perf: {
			status: 'reported',
			steps: 40,
			msPerStep: 3.2,
			layoutMsPerStep: 1.1,
			styleMsPerStep: 0.4,
			scriptMsPerStep: 0.2,
			layoutsPerStep: 2,
			longTasks: 0,
			longestTaskMs: 0,
		},
	}
);

// Different candidate: the leak finding is fixed (resolved), a new overlap
// finding appears, and the overflow/suppressed/error findings are unchanged.
// Evidence differs slightly (different width range/magnitude/count) even for
// the "unchanged" finding, to prove identity — not evidence — drives compare.
const unchangedFindingInCandidate = findingGroup({ kind: 'overflow', container: '#0 .wp-block-tabor-canvas', item: '#1 .canvas__item', worstMagnitude: 11, widthRange: [760, 920], count: 3 });
const candidateSummary = baseSummary(
	[unchangedFindingInCandidate, newInCandidateFinding, suppressedFinding, errorFinding],
	{
		perf: {
			status: 'unavailable',
			message: 'CDP performance domain unavailable in this environment.',
		},
	}
);

// ---------------------------------------------------------------------------
// 1. Declaration validation
// ---------------------------------------------------------------------------

const normalized = normalizeLayoutSweepWorkloadDeclaration(declaration);
assert.equal(normalized.schema, 'homeboy/wordpress-layout-sweep-workload/v1');
assert.equal(normalized.id, 'canvas-grid');
assert.equal(normalized.preview.url, '/canvas-demo/');
assert.equal(normalized.containerSelector, '.wp-block-tabor-canvas');
assert.equal(normalized.itemSelector, ':scope > .canvas__grid > .canvas__item');
assert.equal(normalized.minWidth, 360);
assert.equal(normalized.maxWidth, 1400);
assert.equal(normalized.profile, 'quick');
assert.equal(normalized.seed, 7);
assert.equal(normalized.concurrency, 2);
assert.deepEqual(normalized.scenarios, ['sweep', 'history']);
assert.deepEqual(normalized.accepted, [{ kind: 'tiny-text', container: '#0 .wp-block-tabor-canvas', item: null }]);
assert.equal(normalized.timeout, '120s');

// Defaults apply when only the required fields are supplied.
const minimalNormalized = normalizeLayoutSweepWorkloadDeclaration({
	preview: { url: '/preview' },
	containerSelector: '.container',
	itemSelector: '.item',
});
assert.equal(minimalNormalized.minWidth, 320);
assert.equal(minimalNormalized.maxWidth, 1920);
assert.equal(minimalNormalized.profile, 'quick');
assert.equal(minimalNormalized.seed, 1);
assert.equal(minimalNormalized.concurrency, undefined);
assert.deepEqual(minimalNormalized.scenarios, ['sweep', 'history', 'storm', 'heights']);
assert.deepEqual(minimalNormalized.accepted, []);

assert.throws(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, bogusField: true }), /unsupported field.*bogusField/i);
assert.throws(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, preview: { url: '/x', bogus: true } }), /unsupported field.*bogus/i);
assert.throws(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, accepted: [{ kind: 'overflow', bogus: 1 }] }), /unsupported field.*bogus/i);
assert.throws(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, containerSelector: undefined }), /containerSelector/);
assert.throws(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, itemSelector: '' }), /itemSelector/);
assert.throws(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, preview: undefined }), /preview/);
assert.throws(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, profile: 'slow' }), /profile/);
assert.throws(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, minWidth: 2000, maxWidth: 100 }), /minWidth must not exceed maxWidth/);
assert.throws(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, scenarios: ['not-a-real-scenario'] }), /scenarios must be/);
assert.throws(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, concurrency: 99 }), /concurrency must be from 1 to 8/);
assert.doesNotThrow(() => normalizeLayoutSweepWorkloadDeclaration({ ...declaration, scenarios: ['sweep', 'text-scale:150', 'block-fonts', 'long-text:0.5', 'dpr:2', 'drag'] }));

// ---------------------------------------------------------------------------
// 2. Request building
// ---------------------------------------------------------------------------

const args = buildLayoutSweepCodeboxArgs(declaration);
assert.deepEqual(args, [
	'url=/canvas-demo/',
	'container-selector=.wp-block-tabor-canvas',
	'item-selector=:scope > .canvas__grid > .canvas__item',
	'min-width=360',
	'max-width=1400',
	'profile=quick',
	'seed=7',
	'concurrency=2',
	'scenarios=sweep,history',
	'accepted=[{"kind":"tiny-text","container":"#0 .wp-block-tabor-canvas","item":null}]',
	'timeout=120s',
]);

const minimalArgs = buildLayoutSweepCodeboxArgs({
	preview: { url: '/preview' },
	containerSelector: '.container',
	itemSelector: '.item',
});
assert.deepEqual(minimalArgs, [
	'url=/preview',
	'container-selector=.container',
	'item-selector=.item',
	'min-width=320',
	'max-width=1920',
	'profile=quick',
	'seed=1',
	'scenarios=sweep,history,storm,heights',
]);

const step = buildLayoutSweepRecipeStep(declaration);
assert.equal(step.command, LAYOUT_SWEEP_COMMAND);
assert.deepEqual(step.args, args);

const recipeWithSetup = buildLayoutSweepRecipe(
	{
		...declaration,
		preview: {
			url: '/',
			recipe: {
				runtime: { wp: 'latest' },
				workflow: { steps: [{ command: 'wordpress.plugin-state', args: ['action=activate', 'plugin=tabor/tabor.php'] }] },
			},
		},
	},
	{ artifactDirectory: '/tmp/layout-sweep-artifacts' }
);
assert.equal(recipeWithSetup.schema, 'wp-codebox/workspace-recipe/v1');
assert.equal(recipeWithSetup.runtime.wp, 'latest');
assert.equal(recipeWithSetup.artifacts.directory, '/tmp/layout-sweep-artifacts');
assert.equal(recipeWithSetup.workflow.steps.length, 2);
assert.equal(recipeWithSetup.workflow.steps[0].command, 'wordpress.plugin-state');
assert.equal(recipeWithSetup.workflow.steps[1].command, LAYOUT_SWEEP_COMMAND);
assert.equal(recipeWithSetup.workflow.steps[1].args[0], 'url=/');

const recipeWithoutSetup = buildLayoutSweepRecipe(declaration, { artifactDirectory: '/tmp/layout-sweep-artifacts-2' });
assert.equal(recipeWithoutSetup.workflow.steps.length, 1);
assert.equal(recipeWithoutSetup.workflow.steps[0].command, LAYOUT_SWEEP_COMMAND);

// ---------------------------------------------------------------------------
// 3. Finding mapping: identity, evidence, replay, suppression, error findings
// ---------------------------------------------------------------------------

const baselineFindings = mapLayoutSweepSummaryToFuzzFindings(baselineSummary, { workloadId: 'canvas-grid' });
assert.equal(baselineFindings.length, 4);

const overflowFinding = baselineFindings.find((finding) => finding.kind === 'overflow');
assert.equal(overflowFinding.schema, LAYOUT_SWEEP_FINDING_SCHEMA);
assert.deepEqual(overflowFinding.identity, { kind: 'overflow', container: '#0 .wp-block-tabor-canvas', item: '#1 .canvas__item' });
assert.equal(overflowFinding.status, 'open');
assert.deepEqual(overflowFinding.evidence.width_range, [768, 900]);
assert.equal(overflowFinding.evidence.worst_magnitude, 9);
assert.deepEqual(overflowFinding.evidence.scenarios, ['sweep']);
assert.equal(overflowFinding.evidence.count, 1);
assert.equal(overflowFinding.evidence.sample.kind, 'overflow');
assert.equal(overflowFinding.replay.command, LAYOUT_SWEEP_COMMAND);
assert.equal(overflowFinding.replay.seed, 7);
assert.equal(overflowFinding.replay.profile, 'quick');
assert.deepEqual(overflowFinding.replay.args, ['width=800']);
assert.equal(overflowFinding.metadata.workload_id, 'canvas-grid');
assert.equal(overflowFinding.metadata.source, LAYOUT_SWEEP_COMMAND);
assert.equal(typeof overflowFinding.fingerprint, 'string');
assert.equal(overflowFinding.fingerprint.length > 0, true);

// Accepted groups map to status "suppressed".
const suppressed = baselineFindings.find((finding) => finding.kind === 'tiny-text');
assert.equal(suppressed.status, 'suppressed');

// `error` findings stay findings — same schema/shape as any other kind, not
// diverted to observations.
const errorEntry = baselineFindings.find((finding) => finding.kind === 'error');
assert.ok(errorEntry, 'error findings must be mapped like any other finding kind');
assert.equal(errorEntry.schema, LAYOUT_SWEEP_FINDING_SCHEMA);
assert.equal(errorEntry.status, 'open');

// The `drag` perf block maps to report-only observations, never to findings.
assert.equal(baselineFindings.some((finding) => finding.kind === 'drag'), false);
const baselineObservations = mapLayoutSweepPerfToObservations(baselineSummary, { workloadId: 'canvas-grid' });
assert.equal(baselineObservations.length, 1);
assert.equal(baselineObservations[0].schema, LAYOUT_SWEEP_OBSERVATION_SCHEMA);
assert.equal(baselineObservations[0].kind, 'drag');
assert.equal(baselineObservations[0].report_only, true);
assert.equal(baselineObservations[0].gates, false);
assert.equal(baselineObservations[0].status, 'reported');
assert.equal(baselineObservations[0].metrics.layout_ms_per_step, 1.1);

const candidateObservations = mapLayoutSweepPerfToObservations(candidateSummary);
assert.equal(candidateObservations[0].status, 'unavailable');
assert.equal(candidateObservations[0].message, 'CDP performance domain unavailable in this environment.');

// Workload-result roll-up: suppressed findings do not count toward "open".
const baselineResult = mapLayoutSweepSummaryToWorkloadResult(baselineSummary, { workloadId: 'canvas-grid' });
assert.equal(baselineResult.metrics.layout_sweep_finding_count, 4);
assert.equal(baselineResult.metrics.layout_sweep_suppressed_finding_count, 1);
assert.equal(baselineResult.metrics.layout_sweep_open_finding_count, 3);
assert.equal(baselineResult.metrics.layout_sweep_pass, 0);
assert.equal(baselineResult.observations.length, 1);

// A schema mismatch is rejected clearly.
assert.throws(() => mapLayoutSweepSummaryToFuzzFindings({ schema: 'wp-codebox/other/v1', findings: [] }), /Unsupported layout-sweep summary schema/);

// ---------------------------------------------------------------------------
// 4. Identity fingerprints stay stable across two runs. Regression compare
// (new/resolved/unchanged) is `homeboy fuzz compare`'s job, not this
// module's; this only proves the identity handed to it is stable — the same
// identity produces the same fingerprint even when the evidence around it
// (widthRange, worstMagnitude, count) differs between runs.
// ---------------------------------------------------------------------------

const fingerprintFromBaseline = layoutSweepFindingFingerprint({ kind: 'overflow', container: '#0 .wp-block-tabor-canvas', item: '#1 .canvas__item' });
const fingerprintFromCandidate = layoutSweepFindingFingerprint({ kind: 'overflow', container: '#0 .wp-block-tabor-canvas', item: '#1 .canvas__item' });
assert.equal(fingerprintFromBaseline, fingerprintFromCandidate);
assert.notEqual(fingerprintFromBaseline, layoutSweepFindingFingerprint({ kind: 'leak', container: '#0 .wp-block-tabor-canvas', item: '#1 .canvas__item' }));

const candidateFindings = mapLayoutSweepSummaryToFuzzFindings(candidateSummary, { workloadId: 'canvas-grid' });
const overflowInBaseline = baselineFindings.find((finding) => finding.kind === 'overflow');
const overflowInCandidate = candidateFindings.find((finding) => finding.kind === 'overflow');
assert.equal(overflowInBaseline.fingerprint, overflowInCandidate.fingerprint);
assert.notDeepEqual(overflowInBaseline.evidence, overflowInCandidate.evidence);

const baselineCampaign = buildLayoutSweepFuzzCampaign({ id: 'canvas-grid-trunk', declaration, summary: baselineSummary });
const candidateCampaign = buildLayoutSweepFuzzCampaign({ id: 'canvas-grid-pr', declaration, summary: candidateSummary });
assert.equal(baselineCampaign.schema, 'homeboy/fuzz-campaign/v1');
assert.equal(baselineCampaign.findings.length, 4);
assert.equal(candidateCampaign.findings.length, 4);

// Each mapped finding carries its stable identity in the field Homeboy fuzz
// findings use for identity — `fingerprint` — so `homeboy fuzz compare` can
// match findings across runs by fingerprint alone.
for (const finding of [...baselineCampaign.findings, ...candidateCampaign.findings]) {
	assert.equal(typeof finding.fingerprint, 'string');
	assert.ok(finding.fingerprint.length > 0);
}

console.log('wordpress layout-sweep workload smoke passed');

// modeProperty passes the component's breakpoint signal through to Codebox.
{
	const withMode = buildLayoutSweepCodeboxArgs({ schema: 'homeboy/wordpress-layout-sweep-workload/v1', preview: { url: '/p' }, containerSelector: '.c', itemSelector: '.i', modeProperty: '--canvas-viewport' });
	assert.ok(withMode.includes('mode-property=--canvas-viewport'));
	const withoutMode = buildLayoutSweepCodeboxArgs({ schema: 'homeboy/wordpress-layout-sweep-workload/v1', preview: { url: '/p' }, containerSelector: '.c', itemSelector: '.i' });
	assert.ok(!withoutMode.some((arg) => arg.startsWith('mode-property=')));
	assert.throws(() => buildLayoutSweepCodeboxArgs({ schema: 'homeboy/wordpress-layout-sweep-workload/v1', preview: { url: '/p' }, containerSelector: '.c', itemSelector: '.i', modeProperty: 'color' }), /custom property/);
	console.log('wordpress layout-sweep modeProperty smoke passed');
}

// Every mapped finding carries the fields Homeboy core requires on homeboy/fuzz-finding/v1.
{
	const mapped = mapLayoutSweepSummaryToFuzzFindings({ schema: 'wp-codebox/layout-sweep/v1', seed: 7, profile: 'quick', replay: { args: [] }, findings: [
		{ kind: 'overflow', container: '#4 alignfull', item: '#2 wp-block-heading "Freely"', widthRange: [320, 1920], worstMagnitude: 104, scenarios: ['sweep'], count: 3 },
		{ kind: 'hscroll', container: null, item: null, widthRange: [320, 320], worstMagnitude: 35, scenarios: ['sweep'], count: 1 },
	] }, { workloadId: 'canvas-preview' });
	for (const finding of mapped) {
		for (const field of ['id', 'title', 'severity', 'status']) {
			assert.equal(typeof finding[field], 'string', `finding.${field} is required by homeboy/fuzz-finding/v1`);
			assert.ok(finding[field].length > 0);
		}
	}
	assert.equal(mapped[0].title, 'Layout overflow: #4 alignfull / #2 wp-block-heading "Freely"');
	assert.equal(mapped[1].title, 'Layout hscroll');
	console.log('wordpress layout-sweep required finding fields smoke passed');
}
