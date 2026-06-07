'use strict';

const assert = require('node:assert/strict');

const {
	agentAuthoredBlockMetrics,
	agentSuccessGate,
	evaluateMaterializedSiteQuality,
	importerBlockQualityFailureDetails,
	importerBlockQualityFailureReasons,
	importerBlockQualityMetrics,
	materializedSiteQualityMetrics,
	nativeBlockQualityMetrics,
	visualEditorParityFailureDetails,
	visualEditorParityFailureReasons,
	visualEditorParityMetrics,
	visualPixelDiffFailureDetails,
} = require('../lib/materialized-site-quality');

const cleanResult = { success: true, error: null, timedOut: false };
const cleanImportReport = {
	report: { quality: { core_html_block_count: 0, freeform_block_count: 0, fallback_count: 0, invalid_block_count: 0 } },
};
const cleanVisualComparison = {
	pixel_diff_ratio: 0.01,
	visual_editor_vs_source_pixel_diff_ratio: 0.01,
	visual_editor_vs_frontend_pixel_diff_ratio: 0.01,
	visual_editor_parity_error_count: 0,
};

assert.deepEqual(importerBlockQualityMetrics(cleanImportReport), {
	importerCoreHtmlBlockCount: 0,
	importerFreeformBlockCount: 0,
	importerFallbackCount: 0,
});
assert.deepEqual(importerBlockQualityFailureDetails(importerBlockQualityMetrics(cleanImportReport)), []);
assert.deepEqual(importerBlockQualityFailureReasons(importerBlockQualityMetrics(cleanImportReport)), []);

const importReportWithFallbacks = {
	report: { quality: { core_html_block_count: 2, freeform_block_count: 1, fallback_count: 3, invalid_block_count: 4 } },
};
const importerBlockQuality = importerBlockQualityMetrics(importReportWithFallbacks);
assert.deepEqual(importerBlockQualityFailureDetails(importerBlockQuality), [
	'importer block quality: core/html=2, freeform=1, fallback=3',
]);
assert.deepEqual(importerBlockQualityFailureReasons(importerBlockQuality), [
	'importer_core_html_blocks',
	'importer_freeform_blocks',
	'importer_fallback_blocks',
]);

const visualEditorParity = visualEditorParityMetrics({
	pixel_diff_ratio: 0.01,
	visual_editor_vs_source_pixel_diff_ratio: 0.02,
	visual_editor_vs_frontend_pixel_diff_ratio: 0.08,
	visual_editor_parity_error_count: 0,
});
assert.deepEqual(visualEditorParityFailureReasons(visualEditorParity), ['editor_frontend_visual_diff']);
assert.deepEqual(visualEditorParityFailureDetails(visualEditorParity), [
	'editor render diverges from frontend (editor diff: 0.02, frontend diff: 0.01) - likely block-validation or unscoped CSS',
]);
assert.deepEqual(visualPixelDiffFailureDetails({ pixel_diff_ratio: 0.10 }), [
	'visual pixel diff: 0.100 (threshold: 0.050)',
]);
assert.deepEqual(visualPixelDiffFailureDetails({ pixel_diff_ratio: 0.10 }, { visualPixelDiffThreshold: 0.11 }), []);

assert.deepEqual(agentAuthoredBlockMetrics({
	toolCalls: [
		{ name: 'Write', input: { content: '<!-- wp:html --><section></section><!-- /wp:html -->' } },
		{ name: 'Read', input: { content: '<!-- wp:html -->' } },
	],
}), {
	agent_authored_wp_html_openers: 1,
	agent_authored_wp_html_write_calls: 1,
	agent_authored_wp_block_comments: 2,
});

assert.deepEqual(nativeBlockQualityMetrics(
	{ target_pages_seen: 1, target_posts_with_blocks: 1, bfb_fallback_count: 0, core_html_without_bfb_fallback: 0 },
	{ agent_authored_wp_html_openers: 0 },
	{ invalid_blocks: 0 },
	cleanImportReport
), {
	native_block_quality_pass: true,
	native_block_quality_failure_count: 0,
	native_block_quality_failure_reasons: [],
});

assert.deepEqual(nativeBlockQualityMetrics(
	{ target_pages_seen: 0, target_posts_with_blocks: 0, bfb_fallback_count: 2, core_html_without_bfb_fallback: 1 },
	{ agent_authored_wp_html_openers: 1 },
	{ invalid_blocks: 1, error: 'validation failed' },
	{ ...importReportWithFallbacks, error: 'missing report' }
).native_block_quality_failure_reasons, [
	'missing_target_block_page',
	'agent_authored_wp_html',
	'core_html_without_bfb_fallback',
	'bfb_fallback',
	'importer_core_html_blocks',
	'importer_invalid_blocks',
	'importer_report_error',
	'editor_invalid_blocks',
	'editor_validation_error',
]);

const cleanQualityInput = {
	result: cleanResult,
	semanticComparison: { mismatch_count: 0 },
	importReport: cleanImportReport,
	visualComparison: cleanVisualComparison,
	quality: { target_pages_seen: 1, target_posts_with_blocks: 1, bfb_fallback_count: 0, core_html_without_bfb_fallback: 0 },
	editorValidation: { invalid_blocks: 0 },
};

const cleanMetrics = materializedSiteQualityMetrics(cleanQualityInput);
assert.equal(cleanMetrics.metrics.importer_core_html_block_count, 0);
assert.equal(materializedSiteQualityMetrics({ importReport: importReportWithFallbacks }).metrics.importer_invalid_block_count, 4);
assert.equal(cleanMetrics.metrics.visual_pixel_diff_ratio, 0.01);
assert.equal(cleanMetrics.metrics.native_block_quality_pass, 1);

const cleanGate = evaluateMaterializedSiteQuality(cleanQualityInput);
assert.equal(cleanGate.passed, true);
assert.equal(cleanGate.metrics.success_rate, 1);
assert.deepEqual(cleanGate.failureDetails, []);
assert.deepEqual(cleanGate.failureReasons, []);

const failedGate = evaluateMaterializedSiteQuality({
	...cleanQualityInput,
	semanticComparison: { mismatch_count: 2 },
	importReport: importReportWithFallbacks,
	visualComparison: { ...cleanVisualComparison, pixel_diff_ratio: 0.2 },
	semanticMismatchFailureDetails: () => ['semantic mismatch: headline missing'],
});
assert.equal(failedGate.passed, false);
assert.equal(failedGate.metrics.success_rate, 0);
assert.deepEqual(failedGate.failureDetails, [
	'semantic mismatch: headline missing',
	'importer block quality: core/html=2, freeform=1, fallback=3',
	'visual pixel diff: 0.200 (threshold: 0.050)',
]);
assert.deepEqual(failedGate.failureReasons, [
	'semantic_mismatch',
	'importer_core_html_blocks',
	'importer_freeform_blocks',
	'importer_fallback_blocks',
	'source_frontend_visual_diff',
	'importer_invalid_blocks',
]);

const compatibilityGate = agentSuccessGate(cleanResult, { mismatch_count: 0 }, cleanImportReport, cleanVisualComparison);
assert.equal(compatibilityGate.agentSucceeded, true);
assert.deepEqual(compatibilityGate.importerBlockQualityFailureDetails, []);
assert.deepEqual(compatibilityGate.visualEditorFailureDetails, []);

console.log('materialized site quality smoke passed.');
