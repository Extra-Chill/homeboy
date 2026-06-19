import { createReadStream } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import path from 'node:path';

import {
  createArtifactContext,
  metric,
  resolvePath,
  runNode,
  writeJson,
} from '../../nodejs/scripts/bench/lib/workload-utils.mjs';

/**
 * Run a WordPress/Codebox visual-compare recipe and emit a normalized visual
 * parity artifact for Homeboy bench workloads.
 */
export async function runWordPressCodeboxVisualParityWorkload(options = {}) {
  if (!options || typeof options !== 'object' || Array.isArray(options)) {
    throw new Error('runWordPressCodeboxVisualParityWorkload requires an options object.');
  }

  const id = sanitizeSegment(options.id || 'visual-parity');
  const cwd = resolvePath(options.cwd || process.env.HOMEBOY_COMPONENT_PATH || process.cwd());
  const context = options.artifactContext || createArtifactContext({
    id,
    sharedState: options.sharedState,
    runId: options.runId,
    artifactsDir: options.artifactsDir,
  });
  const artifactDirectory = options.codeboxArtifactsDir
    ? resolvePath(options.codeboxArtifactsDir, { baseDir: cwd })
    : path.join(context.artifactDir, 'codebox');
  const source = normalizeVisualParitySource(options.source, { cwd });
  const candidate = normalizeVisualParityCandidate(options.candidate);
  const compare = normalizeVisualParityCompareOptions(options);
  const backend = normalizeWordPressCodeboxBackend(options.backend || { codeboxCli: options.codeboxCli });
  const recipe = buildVisualParityRecipe({ artifactDirectory, candidate, compare, source });
  const recipePath = context.artifactPath(`${id}-wp-codebox-recipe`, { kind: 'json', extension: 'json' });
  await writeJson(recipePath, recipe, { redact: false });

  const server = source.server ? createStaticFileServer(source.server.root) : undefined;
  if (server) await listen(server, source.server.port);

  let codeboxResult;
  try {
    const result = await runNode([backend.codeboxCli, 'recipe-run', '--recipe', recipePath, '--json'], {
      cwd,
      timeoutMs: options.timeoutMs,
      redact: false,
    });
    codeboxResult = parseJsonOutput(result.stdout, 'WP Codebox recipe output');
  } finally {
    if (server) server.close();
  }

  const visualDiffRef = findVisualCompareArtifactRef(codeboxResult) || 'files/browser/visual-compare/visual-diff.json';
  const visualDiffPath = path.join(artifactDirectory, visualDiffRef);
  const visualDiff = parseJsonOutput(await readFile(visualDiffPath, 'utf8'), visualDiffPath);
  const normalized = normalizeVisualParityArtifact({
    artifactDirectory,
    candidate,
    codeboxResult,
    compare,
    recipePath,
    source,
    visualDiff,
    visualDiffRef,
  });
  const artifact = await context.writeJson(options.artifactName || 'visual-parity-artifact', normalized, {
    label: options.artifactLabel || 'Visual parity artifact',
    kind: 'visual-parity-artifact',
    redact: false,
  });

  return {
    metrics: {
      visual_parity_pass: normalized.summary.pass ? 1 : 0,
      visual_parity_mismatch_ratio: metric(normalized.summary.mismatch_ratio),
      visual_parity_mismatch_pixels: metric(normalized.summary.mismatch_pixels),
      visual_parity_total_pixels: metric(normalized.summary.total_pixels),
      visual_parity_dimension_mismatch: normalized.summary.dimension_mismatch ? 1 : 0,
    },
    artifacts: {
      visualParity: artifact,
    },
    metadata: {
      visual_parity_schema: normalized.schema,
      visual_parity_status: normalized.summary.status,
      visual_parity_threshold: normalized.summary.threshold,
      codebox_recipe: recipePath,
    },
  };
}

function sanitizeSegment(value) {
  const segment = String(value || 'workload')
    .trim()
    .replace(/[^A-Za-z0-9._-]+/g, '-')
    .replace(/^-+|-+$/g, '');
  return segment || 'workload';
}

function normalizeVisualParitySource(source, options = {}) {
  if (!source || typeof source !== 'object' || Array.isArray(source)) {
    throw new Error('runWordPressCodeboxVisualParityWorkload requires source to be an object.');
  }
  const label = String(source.label || source.ref || 'source');
  if (source.url) {
    return { label, ref: source.ref || null, url: String(source.url), path: source.path || null };
  }
  if (!source.path) {
    throw new Error('runWordPressCodeboxVisualParityWorkload source requires url or path.');
  }
  const root = resolvePath(source.path, { baseDir: options.cwd });
  const port = Number(source.port || source.serverPort || 4173);
  if (!Number.isInteger(port) || port <= 0) {
    throw new Error(`runWordPressCodeboxVisualParityWorkload source port must be a positive integer: ${source.port}`);
  }
  const entry = source.entry || 'index.html';
  return {
    label,
    ref: source.ref || null,
    path: root,
    url: `http://127.0.0.1:${port}/${String(entry).replace(/^\/+/, '')}`,
    server: { root, port },
  };
}

function normalizeVisualParityCandidate(candidate) {
  if (!candidate || typeof candidate !== 'object' || Array.isArray(candidate)) {
    throw new Error('runWordPressCodeboxVisualParityWorkload requires candidate to be an object.');
  }
  if (!candidate.url) {
    throw new Error('runWordPressCodeboxVisualParityWorkload candidate requires url.');
  }
  return {
    label: String(candidate.label || candidate.ref || 'candidate'),
    ref: candidate.ref || null,
    url: String(candidate.url),
    recipe: candidate.recipe && typeof candidate.recipe === 'object' && !Array.isArray(candidate.recipe) ? candidate.recipe : {},
    context: candidate.context && typeof candidate.context === 'object' && !Array.isArray(candidate.context) ? candidate.context : {},
  };
}

function normalizeVisualParityCompareOptions(options) {
  const viewport = normalizeViewport(options.viewport || { width: options.width, height: options.height });
  const threshold = Number(options.threshold ?? options.maxMismatchRatio ?? 0.015);
  if (!Number.isFinite(threshold) || threshold < 0 || threshold > 1) {
    throw new Error(`runWordPressCodeboxVisualParityWorkload threshold must be between 0 and 1: ${threshold}`);
  }
  return {
    viewport,
    threshold,
    pixelThreshold: Number(options.pixelThreshold ?? 0.1),
    fullPage: options.fullPage !== false,
    waitFor: String(options.waitFor || 'domcontentloaded'),
    includeAA: Boolean(options.includeAA),
    maxRegions: positiveInteger(options.maxRegions, 8),
  };
}

function normalizeViewport(viewport) {
  const width = Number(viewport?.width ?? 1280);
  const height = Number(viewport?.height ?? 1600);
  if (!Number.isInteger(width) || width <= 0 || !Number.isInteger(height) || height <= 0) {
    throw new Error(`runWordPressCodeboxVisualParityWorkload viewport must include positive integer width and height: ${JSON.stringify(viewport)}`);
  }
  return { width, height };
}

function normalizeWordPressCodeboxBackend(backend) {
  if (!backend || typeof backend !== 'object' || Array.isArray(backend)) {
    throw new Error('runWordPressCodeboxVisualParityWorkload requires backend.codeboxCli.');
  }
  const codeboxCli = backend.codeboxCli || backend.cli;
  if (!codeboxCli || typeof codeboxCli !== 'string') {
    throw new Error('runWordPressCodeboxVisualParityWorkload requires backend.codeboxCli.');
  }
  return { codeboxCli };
}

function positiveInteger(value, fallback) {
  const parsed = Number(value ?? fallback);
  return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function buildVisualParityRecipe({ artifactDirectory, candidate, compare, source }) {
  const visualCompareStep = {
    command: 'wordpress.visual-compare',
    args: [
      `source-url=${source.url}`,
      `candidate-url=${candidate.url}`,
      `source-label=${source.label}`,
      `candidate-label=${candidate.label}`,
      `viewport=${compare.viewport.width}x${compare.viewport.height}`,
      `full-page=${compare.fullPage ? 'true' : 'false'}`,
      `wait-for=${compare.waitFor}`,
      `threshold=${compare.pixelThreshold}`,
      `include-aa=${compare.includeAA ? 'true' : 'false'}`,
      `max-regions=${compare.maxRegions}`,
    ],
  };
  const base = {
    schema: 'wp-codebox/workspace-recipe/v1',
    workflow: {},
    artifacts: { directory: artifactDirectory },
  };
  const recipe = deepMerge(base, candidate.recipe);
  const setupSteps = Array.isArray(candidate.recipe?.workflow?.steps) ? candidate.recipe.workflow.steps : [];
  return {
    ...recipe,
    workflow: {
      ...(recipe.workflow || {}),
      steps: [...setupSteps, visualCompareStep],
    },
    artifacts: {
      ...(recipe.artifacts || {}),
      directory: artifactDirectory,
    },
  };
}

function normalizeVisualParityArtifact({ artifactDirectory, candidate, codeboxResult, compare, recipePath, source, visualDiff, visualDiffRef }) {
  const comparison = visualDiff.comparison || {};
  const mismatchPixels = metric(comparison.mismatchPixels);
  const totalPixels = metric(comparison.totalPixels);
  const mismatchRatio = totalPixels > 0 ? metric(comparison.mismatchRatio, mismatchPixels / totalPixels) : metric(comparison.mismatchRatio);
  const dimensionMismatch = Boolean(comparison.dimensionMismatch);
  const status = mismatchRatio <= compare.threshold && !dimensionMismatch ? 'passed' : 'failed';
  const files = visualDiff.files || {};
  return {
    schema: 'homeboy/VisualParityArtifact/v1',
    source: {
      label: source.label,
      ref: source.ref,
      path: source.path,
      url: source.url,
    },
    candidate: {
      label: candidate.label,
      ref: candidate.ref,
      url: candidate.url,
      context: candidate.context,
    },
    summary: {
      status,
      pass: status === 'passed',
      threshold: compare.threshold,
      mismatch_ratio: mismatchRatio,
      mismatch_pixels: mismatchPixels,
      total_pixels: totalPixels,
      dimension_mismatch: dimensionMismatch,
      region_count: Array.isArray(comparison.regions) ? comparison.regions.length : 0,
    },
    viewport: visualDiff.viewport || compare.viewport,
    options: {
      wait_for: compare.waitFor,
      full_page: compare.fullPage,
      pixel_threshold: compare.pixelThreshold,
      include_aa: compare.includeAA,
      max_regions: compare.maxRegions,
    },
    artifacts: {
      directory: artifactDirectory,
      visual_diff: visualDiffRef,
      source_screenshot: files.sourceScreenshot || null,
      candidate_screenshot: files.candidateScreenshot || null,
      diff_screenshot: files.diffScreenshot || null,
      summary: files.summary || null,
      explanation: files.visualExplanation || null,
    },
    codebox: {
      schema: visualDiff.schema || null,
      status: visualDiff.status || null,
      recipe: recipePath,
      success: codeboxResult?.success === true,
    },
    raw: {
      comparison,
      limitations: visualDiff.limitations || [],
    },
  };
}

function findVisualCompareArtifactRef(codeboxResult) {
  const commands = Array.isArray(codeboxResult?.commands) ? codeboxResult.commands : [];
  for (const command of commands) {
    const artifact = command?.artifact || command?.result?.artifact;
    const ref = artifact?.files?.visualDiff;
    if (typeof ref === 'string' && ref) return ref;
  }
  return undefined;
}

function parseJsonOutput(value, label) {
  try {
    return JSON.parse(value);
  } catch (error) {
    throw new Error(`Unable to parse ${label} as JSON: ${error.message}`);
  }
}

function deepMerge(base, override) {
  if (!override || typeof override !== 'object' || Array.isArray(override)) return base;
  const merged = { ...base };
  for (const [key, value] of Object.entries(override)) {
    if (value && typeof value === 'object' && !Array.isArray(value) && base[key] && typeof base[key] === 'object' && !Array.isArray(base[key])) {
      merged[key] = deepMerge(base[key], value);
    } else {
      merged[key] = value;
    }
  }
  return merged;
}

function createStaticFileServer(root) {
  const contentTypes = new Map([
    ['.css', 'text/css; charset=utf-8'],
    ['.html', 'text/html; charset=utf-8'],
    ['.js', 'text/javascript; charset=utf-8'],
    ['.json', 'application/json; charset=utf-8'],
    ['.png', 'image/png'],
    ['.jpg', 'image/jpeg'],
    ['.jpeg', 'image/jpeg'],
    ['.svg', 'image/svg+xml'],
    ['.webp', 'image/webp'],
  ]);
  return createServer((request, response) => {
    const url = new URL(request.url || '/', `http://${request.headers.host || '127.0.0.1'}`);
    const requestedPath = decodeURIComponent(url.pathname === '/' ? '/index.html' : url.pathname);
    const resolved = path.normalize(path.join(root, requestedPath));
    if (!resolved.startsWith(root)) {
      response.writeHead(403);
      response.end('Forbidden');
      return;
    }
    createReadStream(resolved)
      .on('error', () => {
        response.writeHead(404);
        response.end('Not found');
      })
      .once('open', () => {
        response.writeHead(200, { 'content-type': contentTypes.get(path.extname(resolved).toLowerCase()) || 'application/octet-stream' });
      })
      .pipe(response);
  });
}

function listen(server, port) {
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', resolve);
  });
}
