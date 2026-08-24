'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const { spawnSync } = require('node:child_process');
const { canonicalWpCodeboxRuntime } = require('./wp-codebox-recipe-helper');

const ARTIFACT_KEYS = {
  summary: 'browser_summary',
  memory: 'browser_memory',
  performance: 'browser_performance',
  checkpoints: 'browser_checkpoints',
  html: 'browser_html',
  screenshot: 'browser_screenshot',
};

function runWpCodeboxBrowserMetrics(artifactsDirectory, wpCodeboxBin) {
  const wpCodeboxArgs = ['artifacts', 'browser-metrics', '--bundle', artifactsDirectory, '--json'];
  const invocation = canonicalWpCodeboxRuntime(wpCodeboxBin ? { wp_codebox_bin: wpCodeboxBin } : {}).invocation;
  const result = spawnSync(invocation.command, [...invocation.args, ...wpCodeboxArgs], {
    encoding: 'utf8',
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(result.stderr || result.stdout || `wp-codebox artifacts browser-metrics exited with ${result.status}`);
  }

  const parsed = JSON.parse(result.stdout);
  const artifacts = {};
  for (const [name, artifact] of Object.entries(parsed.artifacts || {})) {
    const key = ARTIFACT_KEYS[name];
    if (key) {
      artifacts[key] = artifact;
    }
  }

  return {
    metrics: parsed.metrics || {},
    artifacts,
  };
}

function scenarioReceivesBrowserMetrics(scenario) {
  return scenario && scenario.id !== '__bootstrap';
}

function enrichBenchResultsWithBrowserMetrics(benchResults, artifactsDirectory, wpCodeboxBin) {
  const parsed = runWpCodeboxBrowserMetrics(artifactsDirectory, wpCodeboxBin);
  if (Object.keys(parsed.metrics).length === 0 && Object.keys(parsed.artifacts).length === 0) {
    return benchResults;
  }

  const next = JSON.parse(JSON.stringify(benchResults));
  next.metrics = { ...(next.metrics || {}), ...parsed.metrics };
  next.artifacts = { ...(next.artifacts || {}), ...parsed.artifacts };
  next.scenarios = (next.scenarios || []).map((scenario) => {
    if (!scenarioReceivesBrowserMetrics(scenario)) {
      return scenario;
    }
    return {
      ...scenario,
      metrics: { ...(scenario.metrics || {}), ...parsed.metrics },
      artifacts: { ...(scenario.artifacts || {}), ...parsed.artifacts },
    };
  });
  return next;
}

function cli(argv) {
  const [benchResultsPath, artifactsDirectory, wpCodeboxBin = 'wp-codebox'] = argv;
  if (!benchResultsPath || !artifactsDirectory) {
    throw new Error('Usage: node wp-codebox-browser-metrics.js <bench-results.json> <artifacts-directory> [wp-codebox-bin]');
  }
  const benchResults = JSON.parse(fs.readFileSync(benchResultsPath, 'utf8'));
  process.stdout.write(`${JSON.stringify(enrichBenchResultsWithBrowserMetrics(benchResults, artifactsDirectory, wpCodeboxBin), null, 2)}\n`);
}

if (require.main === module) {
  cli(process.argv.slice(2));
}

module.exports = {
  enrichBenchResultsWithBrowserMetrics,
  runWpCodeboxBrowserMetrics,
};
