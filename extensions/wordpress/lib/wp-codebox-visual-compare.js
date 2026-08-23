'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const { spawnSync } = require('node:child_process');
const { canonicalWpCodeboxRuntime } = require('./wp-codebox-recipe-helper');

function addArg(args, name, value) {
  if (value !== undefined && value !== null && value !== '') {
    args.push('--arg', `${name}=${value}`);
  }
}

function visualCompareArgs(options) {
  const args = ['run', '--command', 'wordpress.visual-compare'];
  addArg(args, 'source-screenshot', options.sourceScreenshot || options.source_screenshot);
  addArg(args, 'candidate-screenshot', options.candidateScreenshot || options.candidate_screenshot);
  addArg(args, 'source-url', options.sourceUrl || options.source_url);
  addArg(args, 'candidate-url', options.candidateUrl || options.candidate_url);
  addArg(args, 'source-dom-snapshot', options.sourceDomSnapshot || options.source_dom_snapshot);
  addArg(args, 'candidate-dom-snapshot', options.candidateDomSnapshot || options.candidate_dom_snapshot);
  addArg(args, 'source-label', options.sourceLabel || options.source_label);
  addArg(args, 'candidate-label', options.candidateLabel || options.candidate_label);
  addArg(args, 'threshold', options.threshold);
  addArg(args, 'viewport', options.viewport);
  addArg(args, 'full-page', options.fullPage ?? options.full_page);
  addArg(args, 'include-aa', options.includeAA ?? options.include_aa);
  addArg(args, 'wait-for', options.waitFor || options.wait_for);
  addArg(args, 'duration', options.duration);
  addArg(args, 'timeout', options.timeout);
  if (options.baseline) {
    addArg(args, 'baseline', options.baseline);
  }
  for (const selector of options.explainSelectors || options.explain_selectors || []) {
    addArg(args, 'explain-selector', selector);
  }
  args.push('--artifacts', options.artifactsDirectory || options.artifacts_directory);
  args.push('--json');
  return args;
}

function normalizeVisualCompareResult(parsed, artifactsDirectory) {
  const summary = parsed?.summary || parsed?.artifact?.summary?.visualCompare || parsed;
  const comparison = parsed?.comparison || summary?.comparison || parsed?.artifact?.summary?.visualCompare || {};
  const files = parsed?.files || summary?.files || parsed?.artifact?.files || {};
  return {
    schema: parsed?.schema || summary?.schema || 'wp-codebox/visual-compare/v1',
    status: parsed?.status || summary?.status || comparison.status,
    comparison,
    metrics: {
      visual_mismatch_ratio: comparison.mismatchRatio,
      visual_mismatch_pixels: comparison.mismatchPixels,
      visual_total_pixels: comparison.totalPixels,
      visual_dimension_mismatch: comparison.dimensionMismatch,
    },
    artifacts: {
      ...(files.sourceScreenshot ? { visual_source_screenshot: { path: files.sourceScreenshot, kind: 'png' } } : {}),
      ...(files.candidateScreenshot ? { visual_candidate_screenshot: { path: files.candidateScreenshot, kind: 'png' } } : {}),
      ...(files.diffScreenshot ? { visual_diff_screenshot: { path: files.diffScreenshot, kind: 'png' } } : {}),
      ...(files.visualDiff ? { visual_diff_json: { path: files.visualDiff, kind: 'json' } } : {}),
      ...(files.visualExplanation ? { visual_explanation_json: { path: files.visualExplanation, kind: 'json' } } : {}),
      ...(files.summary ? { visual_compare_summary: { path: files.summary, kind: 'json' } } : {}),
    },
    files,
    artifactsDirectory,
    raw: parsed,
  };
}

function runWpCodeboxVisualCompare(options, wpCodeboxBin) {
  if (!options || !(options.artifactsDirectory || options.artifacts_directory)) {
    throw new Error('runWpCodeboxVisualCompare requires artifactsDirectory');
  }
  fs.mkdirSync(options.artifactsDirectory || options.artifacts_directory, { recursive: true });
  const args = visualCompareArgs(options);
  const invocation = canonicalWpCodeboxRuntime(wpCodeboxBin ? { wp_codebox_bin: wpCodeboxBin } : {}).invocation;
  const result = spawnSync(invocation.command, [...invocation.args, ...args], {
    encoding: 'utf8',
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(result.stderr || result.stdout || `wp-codebox visual compare exited with ${result.status}`);
  }
  return normalizeVisualCompareResult(JSON.parse(result.stdout), options.artifactsDirectory || options.artifacts_directory);
}

function cli(argv) {
  const [inputPath, wpCodeboxBin = 'wp-codebox'] = argv;
  if (!inputPath) {
    throw new Error('Usage: node wp-codebox-visual-compare.js <input.json> [wp-codebox-bin]');
  }
  const input = JSON.parse(fs.readFileSync(inputPath, 'utf8'));
  process.stdout.write(`${JSON.stringify(runWpCodeboxVisualCompare(input, wpCodeboxBin), null, 2)}\n`);
}

if (require.main === module) {
  cli(process.argv.slice(2));
}

module.exports = {
  normalizeVisualCompareResult,
  runWpCodeboxVisualCompare,
  visualCompareArgs,
};
