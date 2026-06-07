'use strict';

/* eslint-disable no-console */

const assert = require('node:assert/strict');
const { mkdtemp, rm, writeFile } = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');

const {
	buildSemanticArtifact,
	buildVisualDiagnostics,
	comparePngScreenshots,
	compareSemanticFingerprints,
	comparisonTargets,
	encodePng,
	resolveSourceStaticFile,
	semanticComparisonTargets,
	semanticMismatchFailureDetails,
	semanticTargetMetric,
	semanticTargetSelectorGroups,
	surfaceUrl,
	visualParity,
	visualProbeGroups,
	visualSelectorComparisonDetails,
	visualSurfaceTotals,
} = require('../lib/fidelity-comparison');

async function main() {
	const importReport = {
		reportPath: '/tmp/report/import-report.json',
		report: {
			visual_fidelity: {
				comparison_targets: [
					{
						source_filename: 'index.html',
						source_file: 'source/index.html',
						wordpress_page_id: 12,
						wordpress_url: 'https://example.test/home/',
						comparison_hooks: {
							hero: ['.hero'],
							visible_chrome: ['header'],
							layout_probes: {
								nav_chrome: { selectors: ['nav'] },
							},
						},
					},
				],
			},
		},
	};

	const target = comparisonTargets(importReport)[0];
	assert.equal(semanticComparisonTargets(importReport).length, 1);
	assert.equal(resolveSourceStaticFile('/wordpress/wp-content/source.html', '/tmp/report/report.json', '/site'), '/site/wp-content/source.html');
	assert.equal(resolveSourceStaticFile('source.html', '/tmp/report/report.json', ''), '/tmp/report/source.html');
	assert.equal(surfaceUrl(target, 'wordpress_frontend', importReport.reportPath, ''), 'https://example.test/home/');
	assert.equal(surfaceUrl(target, 'wordpress_editor', importReport.reportPath, ''), 'https://example.test/studio-auto-login?redirect_to=%2Fwp-admin%2Fpost.php%3Fpost%3D12%26action%3Dedit');
	assert.deepEqual(visualProbeGroups(target).map((group) => group.name), ['nav_chrome', 'hero_probe', 'visible_chrome', 'footer_chrome']);
	assert.deepEqual(semanticTargetSelectorGroups(target).map((group) => group.name), ['nav_chrome', 'hero', 'visible_chrome', 'footer_chrome', 'brand_hooks', 'interaction_hooks']);

	const sourceGroups = [
		{
			name: 'hero_probe',
			selectors: [{ selector: '.hero', count: 1, visible_count: 1, nonzero_bounding_box_count: 1, first_match: { visible: true, text: 'Hero', boundingBox: { width: 100, height: 50 } } }],
			selector_count: 1,
			missing_selector_count: 0,
			errored_selector_count: 0,
			matched_selector_count: 1,
			visible_selector_count: 1,
			nonzero_bounding_box_selector_count: 1,
		},
	];
	const frontendGroups = [
		{
			name: 'hero_probe',
			selectors: [{ selector: '.hero', count: 1, visible_count: 0, nonzero_bounding_box_count: 1, first_match: { visible: false, text: 'Hero', boundingBox: { width: 100, height: 50 } } }],
			selector_count: 1,
			missing_selector_count: 0,
			errored_selector_count: 0,
			matched_selector_count: 1,
			visible_selector_count: 0,
			nonzero_bounding_box_selector_count: 1,
		},
	];

	assert.equal(visualSurfaceTotals(sourceGroups).visible_selector_count, 1);
	assert.equal(visualParity(sourceGroups, frontendGroups).visibility_mismatch_count, 1);
	const visualDetails = visualSelectorComparisonDetails({ surfaces: { source_static: { probes: sourceGroups }, wordpress_frontend: { probes: frontendGroups } } });
	assert.equal(visualDetails.mismatches[0].reason, 'hidden_on_wordpress_frontend');
	assert.equal(buildVisualDiagnostics([{ source_filename: 'index.html', diagnostics: { mismatches: visualDetails.mismatches } }], '/tmp/visual.json').mismatch_count, 1);

	const semanticSource = {
		landmarks: { header: { visible_count: 1 }, nav: { visible_count: 1 }, main: { visible_count: 1 }, footer: { visible_count: 1 } },
		class_owners: [{ own_classes: ['cta-button'], role: 'link', href: '/buy', text: 'Buy now', clickable_descendant_count: 1, ancestor_region: 'main', concept: 'button' }],
		regions: { header: { brand_present: true, logo_present: true, link_count: 1, clickable_area: 100 } },
		repeated: { card: 6 },
		selector_groups: [{ name: 'hero', selectors: [{ selector: '.optional', count: 0 }] }],
	};
	const semanticFrontend = {
		landmarks: { header: { visible_count: 0 }, nav: { visible_count: 1 }, main: { visible_count: 1 }, footer: { visible_count: 1 } },
		class_owners: [{ own_classes: ['cta-button'], role: 'group', href: '', text: 'Different', clickable_descendant_count: 0, ancestor_region: 'main', concept: 'button' }],
		regions: { header: { brand_present: true, logo_present: false, link_count: 0, clickable_area: 20 } },
		repeated: { card: 1 },
		selector_groups: [{ name: 'hero', selectors: [{ selector: '.optional', count: 0 }] }],
	};
	const semanticComparison = compareSemanticFingerprints(semanticSource, semanticFrontend);
	assert.equal(semanticComparison.landmark_mismatch_count, 1);
	assert.equal(semanticComparison.role_mismatch_count, 1);
	assert.equal(semanticComparison.brand_logo_missing_count, 1);
	assert.equal(semanticComparison.repeated_count_delta_count, 1);
	assert.equal(semanticComparison.optional_selector_absences.length, 1);

	const semanticArtifact = buildSemanticArtifact([{ source_filename: 'index.html', comparison: semanticComparison }], '/tmp/semantic.json');
	assert.equal(semanticTargetMetric({ diagnostics: semanticArtifact }, 'role_mismatch_count'), 1);
	assert.equal(semanticMismatchFailureDetails({ diagnostics: semanticArtifact }).some((detail) => detail.includes('landmark_disappeared')), true);

	const tmpDir = await mkdtemp(path.join(os.tmpdir(), 'homeboy-fidelity-'));
	try {
		const sourcePath = path.join(tmpDir, 'source.png');
		const targetPath = path.join(tmpDir, 'target.png');
		const diffPath = path.join(tmpDir, 'diff.png');
		await writeFile(sourcePath, encodePng({ width: 1, height: 1, data: Uint8ClampedArray.from([255, 255, 255, 255]) }));
		await writeFile(targetPath, encodePng({ width: 1, height: 1, data: Uint8ClampedArray.from([0, 0, 0, 255]) }));
		const pngComparison = await comparePngScreenshots(sourcePath, targetPath, diffPath);
		assert.equal(pngComparison.mismatched_pixels, 1);
		assert.equal(pngComparison.ratio, 1);
	} finally {
		await rm(tmpDir, { recursive: true, force: true });
	}

	console.log('WordPress fidelity comparison smoke passed.');
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
