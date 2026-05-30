'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

const ARTIFACTS = {
  summary: { file: 'summary.json', key: 'browser_summary', kind: 'json' },
  memory: { file: 'memory.json', key: 'browser_memory', kind: 'json' },
  performance: { file: 'performance.json', key: 'browser_performance', kind: 'json' },
  checkpoints: { file: 'checkpoints.jsonl', key: 'browser_checkpoints', kind: 'jsonl' },
};

function readJsonIfPresent(filePath) {
  if (!fs.existsSync(filePath) || fs.statSync(filePath).size === 0) {
    return null;
  }
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function readJsonlIfPresent(filePath) {
  if (!fs.existsSync(filePath) || fs.statSync(filePath).size === 0) {
    return [];
  }
  return fs.readFileSync(filePath, 'utf8')
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

function numberValue(value) {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === 'string' && value.trim() !== '') {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function valueAt(object, selector) {
  if (!object || typeof object !== 'object') {
    return undefined;
  }
  return selector.split('.').reduce((current, segment) => {
    if (!current || typeof current !== 'object') {
      return undefined;
    }
    return current[segment];
  }, object);
}

function firstNumber(sources, selectors) {
  for (const source of sources) {
    for (const selector of selectors) {
      const value = numberValue(valueAt(source, selector));
      if (value !== null) {
        return value;
      }
    }
  }
  return null;
}

function maxSampleNumber(samples, selectors) {
  const values = samples.flatMap((sample) => selectors.map((selector) => numberValue(valueAt(sample, selector))))
    .filter((value) => value !== null);
  return values.length > 0 ? Math.max(...values) : null;
}

function lastSampleNumber(samples, selectors, labelPattern = null) {
  const filtered = labelPattern
    ? samples.filter((sample) => labelPattern.test(String(sample.label || sample.name || sample.checkpoint || sample.phase || '')))
    : samples;
  for (let index = filtered.length - 1; index >= 0; index -= 1) {
    for (const selector of selectors) {
      const value = numberValue(valueAt(filtered[index], selector));
      if (value !== null) {
        return value;
      }
    }
  }
  return null;
}

function sumArrayNumbers(items, selectors) {
  const values = items.map((item) => firstNumber([item], selectors)).filter((value) => value !== null);
  return values.length > 0 ? values.reduce((total, value) => total + value, 0) : null;
}

function listFrom(value, selectors) {
  for (const selector of selectors) {
    const candidate = selector ? valueAt(value, selector) : value;
    if (Array.isArray(candidate)) {
      return candidate;
    }
  }
  return [];
}

function findBrowserDirectory(artifactsDirectory) {
  if (!artifactsDirectory || !fs.existsSync(artifactsDirectory)) {
    return null;
  }

  const directCandidates = [
    path.join(artifactsDirectory, 'files', 'browser'),
    path.join(artifactsDirectory, 'browser'),
  ];
  const direct = directCandidates.find((candidate) => fs.existsSync(candidate) && fs.statSync(candidate).isDirectory());
  if (direct) {
    return direct;
  }

  const queue = [artifactsDirectory];
  while (queue.length > 0) {
    const directory = queue.shift();
    const entries = fs.readdirSync(directory, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      if (!entry.isDirectory()) {
        continue;
      }
      const child = path.join(directory, entry.name);
      if (path.basename(child) === 'browser' && path.basename(path.dirname(child)) === 'files') {
        return child;
      }
      queue.push(child);
    }
  }

  return null;
}

function parseWpCodeboxBrowserArtifacts(artifactsDirectory) {
  const browserDirectory = findBrowserDirectory(artifactsDirectory);
  if (!browserDirectory) {
    return { metrics: {}, artifacts: {} };
  }

  const paths = Object.fromEntries(
    Object.entries(ARTIFACTS).map(([name, artifact]) => [name, path.join(browserDirectory, artifact.file)])
  );
  const summary = readJsonIfPresent(paths.summary) || {};
  const memory = readJsonIfPresent(paths.memory) || {};
  const performance = readJsonIfPresent(paths.performance) || {};
  const checkpoints = readJsonlIfPresent(paths.checkpoints);
  const memorySamples = [
    ...listFrom(memory, ['samples', 'snapshots', 'measurements', 'checkpoints']),
    ...checkpoints,
  ];
  const resources = listFrom(performance, ['resources', 'resourceTimings', 'entries.resources']);
  const longTasks = listFrom(performance, ['longTasks', 'long_tasks', 'entries.longTasks']);
  const sources = [summary, memory, performance, ...checkpoints];

  const metrics = {};
  const setMetric = (key, value) => {
    if (value !== null && value !== undefined) {
      metrics[key] = value;
    }
  };

  setMetric('browser_peak_used_js_heap_bytes', firstNumber(sources, [
    'browser_peak_used_js_heap_bytes',
    'peak_used_js_heap_bytes',
    'peakUsedJSHeapSize',
    'peak.usedJSHeapSize',
    'peak.used_js_heap_bytes',
    'memory.peakUsedJSHeapSize',
    'memory.peak.usedJSHeapSize',
  ]) ?? maxSampleNumber(memorySamples, ['usedJSHeapSize', 'used_js_heap_bytes', 'memory.usedJSHeapSize']));
  setMetric('browser_final_used_js_heap_bytes', firstNumber(sources, [
    'browser_final_used_js_heap_bytes',
    'final_used_js_heap_bytes',
    'finalUsedJSHeapSize',
    'final.usedJSHeapSize',
    'final.used_js_heap_bytes',
    'memory.finalUsedJSHeapSize',
    'memory.final.usedJSHeapSize',
  ]) ?? lastSampleNumber(memorySamples, ['usedJSHeapSize', 'used_js_heap_bytes', 'memory.usedJSHeapSize']));
  setMetric('browser_post_idle_used_js_heap_bytes', firstNumber(sources, [
    'browser_post_idle_used_js_heap_bytes',
    'post_idle_used_js_heap_bytes',
    'postIdleUsedJSHeapSize',
    'postIdle.usedJSHeapSize',
    'post_idle.used_js_heap_bytes',
    'afterIdle.usedJSHeapSize',
  ]) ?? lastSampleNumber(memorySamples, ['usedJSHeapSize', 'used_js_heap_bytes', 'memory.usedJSHeapSize'], /post[-_ ]?idle|after[-_ ]?idle/i));
  setMetric('browser_generation_heap_delta_bytes', firstNumber(sources, [
    'browser_generation_heap_delta_bytes',
    'generation_heap_delta_bytes',
    'generationHeapDeltaBytes',
    'generation.heapDeltaBytes',
    'generation.heap_delta_bytes',
  ]));
  if (!Object.hasOwn(metrics, 'browser_generation_heap_delta_bytes')) {
    const before = firstNumber(sources, ['beforeGeneration.usedJSHeapSize', 'generation.before.usedJSHeapSize', 'initial.usedJSHeapSize']);
    const after = firstNumber(sources, ['afterGeneration.usedJSHeapSize', 'generation.after.usedJSHeapSize', 'final.usedJSHeapSize']);
    if (before !== null && after !== null) {
      metrics.browser_generation_heap_delta_bytes = after - before;
    }
  }
  setMetric('browser_dom_node_count', firstNumber(sources, [
    'browser_dom_node_count',
    'dom_node_count',
    'domNodeCount',
    'dom.nodeCount',
    'counts.domNodes',
    'memory.domNodeCount',
  ]) ?? lastSampleNumber(memorySamples, ['domNodeCount', 'dom_node_count', 'dom.nodeCount']));
  setMetric('browser_iframe_count', firstNumber(sources, [
    'browser_iframe_count',
    'iframe_count',
    'iframeCount',
    'dom.iframeCount',
    'counts.iframes',
  ]));
  setMetric('browser_resource_count', firstNumber(sources, [
    'browser_resource_count',
    'resource_count',
    'resourceCount',
    'resources.count',
    'network.resourceCount',
  ]) ?? (resources.length > 0 ? resources.length : null));
  setMetric('browser_transfer_size_bytes', firstNumber(sources, [
    'browser_transfer_size_bytes',
    'transfer_size_bytes',
    'transferSizeBytes',
    'resources.transferSizeBytes',
    'network.transferSizeBytes',
  ]) ?? sumArrayNumbers(resources, ['transferSize', 'transfer_size', 'transferSizeBytes']));
  setMetric('browser_long_task_count', firstNumber(sources, [
    'browser_long_task_count',
    'long_task_count',
    'longTaskCount',
    'longTasks.count',
  ]) ?? (longTasks.length > 0 ? longTasks.length : null));
  setMetric('browser_long_task_total_ms', firstNumber(sources, [
    'browser_long_task_total_ms',
    'long_task_total_ms',
    'longTaskTotalMs',
    'longTasks.totalMs',
  ]) ?? sumArrayNumbers(longTasks, ['duration', 'durationMs', 'duration_ms']));

  const artifacts = {};
  for (const [name, artifact] of Object.entries(ARTIFACTS)) {
    if (!fs.existsSync(paths[name])) {
      continue;
    }
    artifacts[artifact.key] = {
      path: path.relative(artifactsDirectory, paths[name]),
      kind: artifact.kind,
    };
  }

  return { metrics, artifacts };
}

function scenarioReceivesBrowserMetrics(scenario) {
  return scenario && scenario.id !== '__bootstrap';
}

function enrichBenchResultsWithBrowserMetrics(benchResults, artifactsDirectory) {
  const parsed = parseWpCodeboxBrowserArtifacts(artifactsDirectory);
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
  const [benchResultsPath, artifactsDirectory] = argv;
  if (!benchResultsPath || !artifactsDirectory) {
    throw new Error('Usage: node wp-codebox-browser-metrics.js <bench-results.json> <artifacts-directory>');
  }
  const benchResults = JSON.parse(fs.readFileSync(benchResultsPath, 'utf8'));
  process.stdout.write(`${JSON.stringify(enrichBenchResultsWithBrowserMetrics(benchResults, artifactsDirectory), null, 2)}\n`);
}

if (require.main === module) {
  cli(process.argv.slice(2));
}

module.exports = {
  enrichBenchResultsWithBrowserMetrics,
  parseWpCodeboxBrowserArtifacts,
};
