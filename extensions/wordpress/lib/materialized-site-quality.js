'use strict';

const DEFAULT_VISUAL_PIXEL_DIFF_THRESHOLD = 0.05;
const DEFAULT_VISUAL_EDITOR_PIXEL_DIFF_THRESHOLD = 0.05;

function metric(value) {
	const number = Number(value ?? 0);
	return Number.isFinite(number) ? number : 0;
}

function threshold(value, defaultValue) {
	const number = Number(value ?? defaultValue);
	return Number.isFinite(number) && number >= 0 ? number : defaultValue;
}

function visualRatio(value) {
	const ratio = metric(value);
	return Number.isFinite(ratio) ? ratio : 1;
}

function formatVisualRatio(value) {
	return visualRatio(value).toFixed(2);
}

function importerBlockQualityMetrics(importReport) {
	const quality = importReport?.report?.quality || {};

	return {
		importerCoreHtmlBlockCount: metric(quality.core_html_block_count),
		importerFreeformBlockCount: metric(quality.freeform_block_count),
		importerFallbackCount: metric(quality.fallback_count),
	};
}

function importerBlockQualityFailureDetails(importerBlockQuality) {
	const { importerCoreHtmlBlockCount, importerFreeformBlockCount, importerFallbackCount } = importerBlockQuality;

	if (importerCoreHtmlBlockCount === 0 && importerFreeformBlockCount === 0 && importerFallbackCount === 0) {
		return [];
	}

	return [
		`importer block quality: core/html=${importerCoreHtmlBlockCount}, freeform=${importerFreeformBlockCount}, fallback=${importerFallbackCount}`,
	];
}

function importerBlockQualityFailureReasons(importerBlockQuality) {
	const reasons = [];
	if (metric(importerBlockQuality?.importerCoreHtmlBlockCount) > 0) {
		reasons.push('importer_core_html_blocks');
	}
	if (metric(importerBlockQuality?.importerFreeformBlockCount) > 0) {
		reasons.push('importer_freeform_blocks');
	}
	if (metric(importerBlockQuality?.importerFallbackCount) > 0) {
		reasons.push('importer_fallback_blocks');
	}
	return reasons;
}

function visualEditorParityMetrics(visualComparison) {
	return {
		visualEditorVsSourcePixelDiffRatio: visualRatio(visualComparison?.visual_editor_vs_source_pixel_diff_ratio),
		visualEditorVsFrontendPixelDiffRatio: visualRatio(visualComparison?.visual_editor_vs_frontend_pixel_diff_ratio),
		visualSourceVsFrontendPixelDiffRatio: visualRatio(
			visualComparison?.visual_pixel_diff_ratio ??
				visualComparison?.pixel_diff_ratio ??
				visualComparison?.visual_source_vs_frontend_pixel_diff_ratio_diagnostic
		),
		visualEditorParityErrorCount: metric(visualComparison?.visual_editor_parity_error_count),
	};
}

function visualEditorParityFailureDetails(visualEditorParity, options = {}) {
	const visualEditorPixelDiffThreshold = threshold(
		options.visualEditorPixelDiffThreshold,
		DEFAULT_VISUAL_EDITOR_PIXEL_DIFF_THRESHOLD
	);
	const {
		visualEditorVsSourcePixelDiffRatio,
		visualEditorVsFrontendPixelDiffRatio,
		visualSourceVsFrontendPixelDiffRatio,
		visualEditorParityErrorCount,
	} = visualEditorParity;
	const editorFailedSource = visualEditorVsSourcePixelDiffRatio > visualEditorPixelDiffThreshold;
	const editorFailedFrontend = visualEditorVsFrontendPixelDiffRatio > visualEditorPixelDiffThreshold;

	if (visualEditorParityErrorCount > 0) {
		return [`editor visual parity could not be measured (${visualEditorParityErrorCount} capture/diff errors)`];
	}

	if (!editorFailedSource && !editorFailedFrontend) {
		return [];
	}

	if (editorFailedFrontend && visualSourceVsFrontendPixelDiffRatio <= visualEditorPixelDiffThreshold) {
		return [
			`editor render diverges from frontend (editor diff: ${formatVisualRatio(
				visualEditorVsSourcePixelDiffRatio
			)}, frontend diff: ${formatVisualRatio(
				visualSourceVsFrontendPixelDiffRatio
			)}) - likely block-validation or unscoped CSS`,
		];
	}

	if (editorFailedSource && visualSourceVsFrontendPixelDiffRatio > visualEditorPixelDiffThreshold) {
		return [
			`editor and frontend both diverge from source (editor: ${formatVisualRatio(
				visualEditorVsSourcePixelDiffRatio
			)}, frontend: ${formatVisualRatio(visualSourceVsFrontendPixelDiffRatio)}) - conversion failed before editor concern`,
		];
	}

	return [
		`editor visual parity failed (editor vs source: ${formatVisualRatio(
			visualEditorVsSourcePixelDiffRatio
		)}, editor vs frontend: ${formatVisualRatio(visualEditorVsFrontendPixelDiffRatio)})`,
	];
}

function visualEditorParityFailureReasons(visualEditorParity, options = {}) {
	const visualEditorPixelDiffThreshold = threshold(
		options.visualEditorPixelDiffThreshold,
		DEFAULT_VISUAL_EDITOR_PIXEL_DIFF_THRESHOLD
	);
	const reasons = [];
	if (metric(visualEditorParity?.visualEditorParityErrorCount) > 0) {
		reasons.push('editor_visual_parity_error');
	}
	if (visualRatio(visualEditorParity?.visualEditorVsSourcePixelDiffRatio) > visualEditorPixelDiffThreshold) {
		reasons.push('editor_source_visual_diff');
	}
	if (visualRatio(visualEditorParity?.visualEditorVsFrontendPixelDiffRatio) > visualEditorPixelDiffThreshold) {
		reasons.push('editor_frontend_visual_diff');
	}
	return reasons;
}

function visualPixelDiffFailureDetails(visualComparison, options = {}) {
	const visualPixelDiffThreshold = threshold(options.visualPixelDiffThreshold, DEFAULT_VISUAL_PIXEL_DIFF_THRESHOLD);
	const visualPixelDiffRatio = metric(visualComparison?.pixel_diff_ratio);
	if (visualPixelDiffRatio <= visualPixelDiffThreshold) {
		return [];
	}

	return [
		`visual pixel diff: ${visualPixelDiffRatio.toFixed(3)} (threshold: ${visualPixelDiffThreshold.toFixed(3)})`,
	];
}

function visualPixelDiffFailureReasons(visualComparison, options = {}) {
	const visualPixelDiffThreshold = threshold(options.visualPixelDiffThreshold, DEFAULT_VISUAL_PIXEL_DIFF_THRESHOLD);
	return metric(visualComparison?.pixel_diff_ratio) > visualPixelDiffThreshold ? ['source_frontend_visual_diff'] : [];
}

function agentAuthoredBlockMetrics(result) {
	const toolCalls = Array.isArray(result?.toolCalls) ? result.toolCalls : [];
	const writeCalls = toolCalls.filter((item) => item && item.name === 'Write');
	let agentAuthoredWpHtmlOpeners = 0;
	let agentAuthoredWpBlockComments = 0;
	let agentAuthoredWpHtmlWriteCalls = 0;

	for (const call of writeCalls) {
		const content = typeof call?.input?.content === 'string' ? call.input.content : '';
		const wpHtmlOpeners = countRegex(content, /<!--\s+wp:html\b/g);
		agentAuthoredWpHtmlOpeners += wpHtmlOpeners;
		agentAuthoredWpBlockComments += countRegex(content, /<!--\s+\/?wp:/g);
		if (wpHtmlOpeners > 0) {
			agentAuthoredWpHtmlWriteCalls++;
		}
	}

	return {
		agent_authored_wp_html_openers: agentAuthoredWpHtmlOpeners,
		agent_authored_wp_html_write_calls: agentAuthoredWpHtmlWriteCalls,
		agent_authored_wp_block_comments: agentAuthoredWpBlockComments,
	};
}

function nativeBlockQualityMetrics(quality, authoredBlocks = {}, editorValidation = {}, importReport = {}) {
	const reasons = [];
	const bfbFallbackCount = metric(quality?.bfb_fallback_count);
	const coreHtmlWithoutBfbFallback = metric(quality?.core_html_without_bfb_fallback);
	const importerQuality = importReport?.report?.quality || {};
	const importerCoreHtmlBlocks = metric(importerQuality.core_html_block_count);
	const importerInvalidBlocks = metric(importerQuality.invalid_block_count);
	const agentAuthoredWpHtmlOpeners = metric(authoredBlocks.agent_authored_wp_html_openers);
	const invalidEditorBlocks = metric(editorValidation?.invalid_blocks);
	const targetPagesSeen = metric(quality?.target_pages_seen);
	const targetPostsWithBlocks = metric(quality?.target_posts_with_blocks);

	if (targetPagesSeen === 0 || targetPostsWithBlocks === 0) {
		reasons.push('missing_target_block_page');
	}
	if (agentAuthoredWpHtmlOpeners > 0) {
		reasons.push('agent_authored_wp_html');
	}
	if (coreHtmlWithoutBfbFallback > 0) {
		reasons.push('core_html_without_bfb_fallback');
	}
	if (bfbFallbackCount > 0) {
		reasons.push('bfb_fallback');
	}
	if (importerCoreHtmlBlocks > 0) {
		reasons.push('importer_core_html_blocks');
	}
	if (importerInvalidBlocks > 0) {
		reasons.push('importer_invalid_blocks');
	}
	if (importReport?.error) {
		reasons.push('importer_report_error');
	}
	if (invalidEditorBlocks > 0) {
		reasons.push('editor_invalid_blocks');
	}
	if (editorValidation?.error) {
		reasons.push('editor_validation_error');
	}

	return {
		native_block_quality_pass: reasons.length === 0,
		native_block_quality_failure_count: reasons.length,
		native_block_quality_failure_reasons: reasons,
	};
}

function materializedSiteQualityMetrics(input = {}) {
	const result = input.result || {};
	const semanticComparison = input.semanticComparison || {};
	const importReport = input.importReport || {};
	const visualComparison = input.visualComparison || {};
	const quality = input.quality || {};
	const authoredBlocks = input.authoredBlocks || agentAuthoredBlockMetrics(result);
	const editorValidation = input.editorValidation || {};
	const importerBlockQuality = importerBlockQualityMetrics(importReport);
	const importerInvalidBlockCount = metric(importReport?.report?.quality?.invalid_block_count);
	const visualEditorParity = visualEditorParityMetrics(visualComparison);
	const visualPixelDiffRatio = metric(visualComparison?.pixel_diff_ratio);
	const semanticMismatchCount = metric(semanticComparison?.mismatch_count);
	const agentTimedOut = result?.timedOut === true;
	const agentRunnerError = typeof result?.error === 'string' && result.error.length > 0;
	const nativeBlockQuality = nativeBlockQualityMetrics(quality, authoredBlocks, editorValidation, importReport);

	return {
		semanticMismatchCount,
		importerBlockQuality,
		visualEditorParity,
		visualPixelDiffRatio,
		agentTimedOut,
		agentRunnerError,
		authoredBlocks,
		nativeBlockQuality,
		metrics: {
			semantic_mismatch_count: semanticMismatchCount,
			importer_core_html_block_count: importerBlockQuality.importerCoreHtmlBlockCount,
			importer_freeform_block_count: importerBlockQuality.importerFreeformBlockCount,
			importer_fallback_count: importerBlockQuality.importerFallbackCount,
			importer_invalid_block_count: importerInvalidBlockCount,
			visual_editor_vs_source_pixel_diff_ratio: visualEditorParity.visualEditorVsSourcePixelDiffRatio,
			visual_editor_vs_frontend_pixel_diff_ratio: visualEditorParity.visualEditorVsFrontendPixelDiffRatio,
			visual_source_vs_frontend_pixel_diff_ratio: visualEditorParity.visualSourceVsFrontendPixelDiffRatio,
			visual_editor_parity_error_count: visualEditorParity.visualEditorParityErrorCount,
			visual_pixel_diff_ratio: visualPixelDiffRatio,
			native_block_quality_pass: nativeBlockQuality.native_block_quality_pass ? 1 : 0,
			native_block_quality_failure_count: nativeBlockQuality.native_block_quality_failure_count,
			agent_error_rate: 1,
			timed_out: agentTimedOut ? 1 : 0,
			agent_runner_error: agentRunnerError ? 1 : 0,
		},
	};
}

function evaluateMaterializedSiteQuality(input = {}, options = {}) {
	const metrics = materializedSiteQualityMetrics(input);
	const result = input.result || {};
	const semanticFailureDetails = semanticFailureDetailsForInput(input);
	const failureDetails = [
		...semanticFailureDetails,
		...importerBlockQualityFailureDetails(metrics.importerBlockQuality),
		...visualEditorParityFailureDetails(metrics.visualEditorParity, options),
		...visualPixelDiffFailureDetails(input.visualComparison || {}, options),
	];
	const failureReasons = [
		...(metrics.semanticMismatchCount > 0 ? ['semantic_mismatch'] : []),
		...importerBlockQualityFailureReasons(metrics.importerBlockQuality),
		...visualEditorParityFailureReasons(metrics.visualEditorParity, options),
		...visualPixelDiffFailureReasons(input.visualComparison || {}, options),
		...metrics.nativeBlockQuality.native_block_quality_failure_reasons,
		...(metrics.agentTimedOut ? ['agent_timed_out'] : []),
		...(metrics.agentRunnerError ? ['agent_runner_error'] : []),
	];
	const passed =
		result?.success === true &&
		!result?.error &&
		!metrics.agentTimedOut &&
		metrics.semanticMismatchCount === 0 &&
		failureDetails.length === 0;

	return {
		...metrics,
		passed,
		failureDetails,
		failureReasons: unique(failureReasons),
		metrics: {
			...metrics.metrics,
			success_rate: passed ? 1 : 0,
			agent_error_rate: passed ? 0 : 1,
		},
	};
}

function agentSuccessGate(result, semanticComparison, importReport, visualComparison, options = {}) {
	const gate = evaluateMaterializedSiteQuality({ result, semanticComparison, importReport, visualComparison }, options);
	return {
		agentSucceeded: gate.passed,
		semanticMismatchCount: gate.semanticMismatchCount,
		semanticFailureDetails: semanticFailureDetailsForInput({ semanticComparison }),
		importerBlockQuality: gate.importerBlockQuality,
		importerBlockQualityFailureDetails: importerBlockQualityFailureDetails(gate.importerBlockQuality),
		visualEditorParity: gate.visualEditorParity,
		visualEditorFailureDetails: visualEditorParityFailureDetails(gate.visualEditorParity, options),
		visualPixelDiffRatio: gate.visualPixelDiffRatio,
		visualPixelDiffFailureDetails: visualPixelDiffFailureDetails(visualComparison, options),
		metrics: gate.metrics,
	};
}

function semanticFailureDetailsForInput(input) {
	if (Array.isArray(input.semanticFailureDetails)) {
		return input.semanticFailureDetails;
	}
	if (typeof input.semanticMismatchFailureDetails === 'function') {
		return metric(input.semanticComparison?.mismatch_count) > 0 ? input.semanticMismatchFailureDetails(input.semanticComparison) : [];
	}
	if (metric(input.semanticComparison?.mismatch_count) > 0) {
		return [`semantic mismatch count: ${metric(input.semanticComparison?.mismatch_count)}`];
	}
	return [];
}

function countRegex(value, pattern) {
	return typeof value === 'string' ? (value.match(pattern) || []).length : 0;
}

function unique(values) {
	return [...new Set(values.filter(Boolean))];
}

module.exports = {
	DEFAULT_VISUAL_EDITOR_PIXEL_DIFF_THRESHOLD,
	DEFAULT_VISUAL_PIXEL_DIFF_THRESHOLD,
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
	visualPixelDiffFailureReasons,
};
