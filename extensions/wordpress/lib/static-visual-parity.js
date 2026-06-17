'use strict';

/**
 * External dependencies
 */
const { createReadStream } = require('node:fs');
const { createServer } = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');

/**
 * Internal dependencies
 */
const { runWpCodeboxRecipe } = require('./wp-codebox-recipe-helper');
const { resolveWpCodeboxArtifactPath } = require('./wp-codebox-artifacts');

function buildStaticVisualParityRecipe(options = {}) {
  const sourceUrl = requiredString(options.sourceUrl, 'sourceUrl');
  const candidateUrl = requiredString(options.candidateUrl, 'candidateUrl');
  const artifactsDirectory = requiredString(options.artifactsDirectory, 'artifactsDirectory');
  const viewport = options.viewport || {};
  const width = Number(viewport.width || options.width || 1280);
  const height = Number(viewport.height || options.height || 1600);
  const visualArgs = [
    `source-url=${sourceUrl}`,
    `candidate-url=${candidateUrl}`,
    `source-label=${options.sourceLabel || 'source-static-html'}`,
    `candidate-label=${options.candidateLabel || 'candidate-wordpress'}`,
    `viewport=${width}x${height}`,
    `full-page=${String(options.fullPage ?? true)}`,
    `wait-for=${options.waitFor || 'domcontentloaded'}`,
    `threshold=${options.threshold ?? 0.1}`,
    `include-aa=${String(options.includeAA ?? true)}`,
    ...arrayOption(options.extraVisualArgs),
  ];

  return {
    schema: 'wp-codebox/workspace-recipe/v1',
    runtime: {
      wp: options.wordpressVersion || 'latest',
      blueprint: options.blueprint || {},
    },
    inputs: {
      mounts: arrayOption(options.mounts),
    },
    workflow: {
      steps: [
        {
          command: 'wordpress.visual-compare',
          args: visualArgs,
        },
      ],
    },
    artifacts: {
      directory: artifactsDirectory,
    },
  };
}

async function runStaticVisualParity(options = {}) {
  const outputDirectory = requiredString(options.outputDirectory, 'outputDirectory');
  const artifactsDirectory = options.artifactsDirectory || outputDirectory;
  const sourceDirectory = options.sourceDirectory || '';
  const sourcePort = Number(options.sourcePort || 4173);
  const sourcePath = String(options.sourcePath || '/index.html').replace(/^\/?/, '/');
  const sourceUrl = options.sourceUrl || (sourceDirectory ? `http://127.0.0.1:${sourcePort}${sourcePath}` : '');
  const candidateUrl = options.candidateUrl || '/';
  let sourceServer = null;

  await fs.mkdir(outputDirectory, { recursive: true });
  await fs.mkdir(artifactsDirectory, { recursive: true });

  if (sourceDirectory) {
    sourceServer = createStaticServer(sourceDirectory);
    await listen(sourceServer, sourcePort);
  }

  const recipe = buildStaticVisualParityRecipe({
    ...options,
    sourceUrl,
    candidateUrl,
    artifactsDirectory,
  });
  const recipeFile = options.recipeFile || path.join(outputDirectory, 'wp-codebox-static-visual-parity-recipe.json');
  const codeboxOutputFile = options.codeboxOutputFile || path.join(outputDirectory, 'wp-codebox-output.json');
  await fs.writeFile(recipeFile, `${JSON.stringify(recipe, null, 2)}\n`);

  try {
    const codeboxRun = await runWpCodeboxRecipe({
      recipeFile,
      artifactsDir: artifactsDirectory,
      outputFile: codeboxOutputFile,
      wpCodeboxBin: options.wpCodeboxBin,
      bin: options.bin,
      env: options.env,
      cwd: options.cwd,
      recipeRunArgs: arrayOption(options.recipeRunArgs),
    });
    const codeboxResult = codeboxRun.json;
    if (codeboxResult?.success !== true) {
      throw new Error(`WP Codebox visual compare failed: ${codeboxResult?.error?.message || JSON.stringify(codeboxResult)}`);
    }

    const readiness = await readReadiness(options.readinessFile);
    if (typeof options.validateReadiness === 'function') {
      await options.validateReadiness(readiness);
    }

    const visualDiff = await normalizeStaticVisualParityArtifacts({
      codeboxResult,
      artifactsDirectory,
      outputDirectory,
      maxMismatchRatio: options.maxMismatchRatio,
    });
    const summary = await writeStaticVisualParitySummary({
      outputDirectory,
      sourceUrl,
      candidateUrl,
      readiness,
      visualDiff,
      codeboxResult,
      viewport: options.viewport || { width: Number(options.width || 1280), height: Number(options.height || 1600) },
      metadata: options.metadata || {},
    });

    if (!visualDiff.pass) {
      throw new Error(`Visual parity mismatch ${formatPercent(visualDiff.mismatchRatio)} exceeds threshold ${formatPercent(visualDiff.threshold)}`);
    }

    return { codeboxResult, readiness, recipe, recipeFile, visualDiff, summary };
  } finally {
    if (sourceServer) {
      sourceServer.close();
    }
  }
}

async function normalizeStaticVisualParityArtifacts({ codeboxResult, artifactsDirectory, outputDirectory, maxMismatchRatio } = {}) {
  if (!outputDirectory) {
    throw new Error('normalizeStaticVisualParityArtifacts requires outputDirectory.');
  }
  const sourcePath = path.join(outputDirectory, 'source.png');
  const candidatePath = path.join(outputDirectory, 'candidate.png');
  const importedPath = path.join(outputDirectory, 'imported.png');
  const diffPath = path.join(outputDirectory, 'diff.png');
  const codeboxVisualDiff = JSON.parse(await fs.readFile(resolveWpCodeboxArtifactPath({
    codeboxResult,
    artifactsDirectory: artifactsDirectory || outputDirectory,
    key: 'visualDiff',
    fallbackPath: 'files/browser/visual-compare/visual-diff.json',
  }), 'utf8'));
  const codeboxVisualFiles = codeboxVisualDiff.files || {};
  await fs.copyFile(resolveWpCodeboxArtifactPath({
    codeboxResult,
    artifactsDirectory: artifactsDirectory || outputDirectory,
    artifact: codeboxVisualFiles.sourceScreenshot,
    fallbackPath: 'files/browser/visual-compare/source.png',
  }), sourcePath);
  await fs.copyFile(resolveWpCodeboxArtifactPath({
    codeboxResult,
    artifactsDirectory: artifactsDirectory || outputDirectory,
    artifact: codeboxVisualFiles.candidateScreenshot,
    fallbackPath: 'files/browser/visual-compare/candidate.png',
  }), candidatePath);
  await fs.copyFile(candidatePath, importedPath);
  await fs.copyFile(resolveWpCodeboxArtifactPath({
    codeboxResult,
    artifactsDirectory: artifactsDirectory || outputDirectory,
    artifact: codeboxVisualFiles.diffScreenshot,
    fallbackPath: 'files/browser/visual-compare/diff.png',
  }), diffPath);
  const comparison = codeboxVisualDiff.comparison || {};
  const source = comparison.source || {};
  const candidate = comparison.candidate || {};
  const diff = comparison.diff || {};
  const totalPixels = Number(comparison.totalPixels || 0);
  const mismatchPixels = Number(comparison.mismatchPixels || 0);
  const mismatchRatio = totalPixels > 0 ? Number(comparison.mismatchRatio || mismatchPixels / totalPixels) : 0;
  const dimensionMismatch = Boolean(comparison.dimensionMismatch);
  const threshold = Number(maxMismatchRatio ?? 0.015);
  const result = {
    pass: mismatchRatio <= threshold && !dimensionMismatch,
    threshold,
    mismatchPixels,
    totalPixels,
    mismatchRatio,
    dimensionMismatch,
    regions: visualMismatchRegions(comparison.regions || []),
    source: {
      path: path.basename(sourcePath),
      width: Number(source.width || 0),
      height: Number(source.height || 0),
      probes: [],
    },
    imported: {
      path: path.basename(importedPath),
      width: Number(candidate.width || 0),
      height: Number(candidate.height || 0),
      probes: [],
    },
    candidate: {
      path: path.basename(candidatePath),
      width: Number(candidate.width || 0),
      height: Number(candidate.height || 0),
      probes: [],
    },
    diff: {
      path: path.basename(diffPath),
      width: Number(diff.width || 0),
      height: Number(diff.height || 0),
    },
    codeboxVisualCompare: {
      schema: codeboxVisualDiff.schema,
      status: codeboxVisualDiff.status,
      files: codeboxVisualDiff.files,
      artifactDirectory: codeboxResult?.artifacts?.directory || artifactsDirectory || outputDirectory,
    },
  };

  await fs.writeFile(path.join(outputDirectory, 'visual-diff.json'), `${JSON.stringify(result, null, 2)}\n`);
  return result;
}

async function writeStaticVisualParitySummary({ outputDirectory, sourceUrl, candidateUrl, readiness, visualDiff, codeboxResult, viewport, metadata = {} } = {}) {
  const comparisonHtml = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Static visual parity</title>
<style>
body { margin: 0; font: 14px/1.5 system-ui, sans-serif; color: #1f2937; background: #f3f4f6; }
header { padding: 24px; background: #111827; color: white; }
main { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; padding: 16px; }
section { background: white; border: 1px solid #d1d5db; border-radius: 12px; overflow: hidden; }
h2 { margin: 0; padding: 12px 16px; background: #e5e7eb; font-size: 16px; }
img { display: block; width: 100%; height: auto; }
code { color: #d1d5db; }
@media (max-width: 900px) { main { grid-template-columns: 1fr; } }
</style>
</head>
<body>
<header>
<h1>Static visual parity</h1>
<p>Source: <code>${escapeHtml(sourceUrl)}</code></p>
<p>Candidate: <code>${escapeHtml(candidateUrl)}</code></p>
</header>
<main>
<section><h2>Source static HTML</h2><img src="source.png" alt="Source static HTML screenshot"></section>
<section><h2>Candidate WordPress</h2><img src="imported.png" alt="Candidate WordPress screenshot"></section>
<section><h2>Pixel diff</h2><img src="diff.png" alt="Visual parity diff screenshot"></section>
</main>
</body>
</html>
`;
  await fs.writeFile(path.join(outputDirectory, 'comparison.html'), comparisonHtml);
  const summary = {
    ...metadata,
    sourceUrl,
    importedUrl: candidateUrl,
    candidateUrl,
    readiness,
    importReadiness: readiness,
    viewport,
    visualDiff,
    artifacts: ['source.png', 'imported.png', 'candidate.png', 'diff.png', 'visual-diff.json', 'comparison.html'],
    codeboxOutput: {
      schema: codeboxResult?.schema,
      artifacts: codeboxResult?.artifacts,
    },
  };
  await fs.writeFile(path.join(outputDirectory, 'summary.json'), `${JSON.stringify(summary, null, 2)}\n`);
  return summary;
}

async function readReadiness(readinessFile) {
  if (!readinessFile) {
    return null;
  }
  return JSON.parse(await fs.readFile(readinessFile, 'utf8'));
}

function createStaticServer(root) {
  const resolvedRoot = path.resolve(root);
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
    const resolvedPath = path.resolve(path.join(resolvedRoot, `.${requestedPath}`));
    if (!resolvedPath.startsWith(`${resolvedRoot}${path.sep}`) && resolvedPath !== resolvedRoot) {
      response.writeHead(403);
      response.end('Forbidden');
      return;
    }

    fs.stat(resolvedPath).then((stat) => {
      if (!stat.isFile()) {
        response.writeHead(404);
        response.end('Not found');
        return;
      }
      response.writeHead(200, {
        'content-type': contentTypes.get(path.extname(resolvedPath).toLowerCase()) || 'application/octet-stream',
      });
      createReadStream(resolvedPath).pipe(response);
    }).catch(() => {
      response.writeHead(404);
      response.end('Not found');
    });
  });
}

function listen(server, port) {
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', resolve);
  });
}

function visualMismatchRegions(regions) {
  return regions.slice(0, 8).map((region, index) => {
    const width = Number(region.width || 0);
    const height = Number(region.height || 0);
    const mismatchPixels = Number(region.mismatchPixels || region.pixels || 0);
    const totalPixels = width * height;
    return {
      rank: index + 1,
      x: Number(region.x || 0),
      y: Number(region.y || 0),
      width,
      height,
      mismatchPixels,
      totalPixels,
      mismatchRatio: totalPixels > 0 ? mismatchPixels / totalPixels : 0,
      source_matches: [],
      imported_matches: [],
      layout_deltas: [],
    };
  });
}

function arrayOption(value) {
  return Array.isArray(value) ? value : [];
}

function requiredString(value, name) {
  if (typeof value !== 'string' || value === '') {
    throw new Error(`buildStaticVisualParityRecipe requires ${name}.`);
  }
  return value;
}

function formatPercent(value) {
  return `${(Number(value || 0) * 100).toFixed(2)}%`;
}

function escapeHtml(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;');
}

module.exports = {
  buildStaticVisualParityRecipe,
  createStaticServer,
  normalizeStaticVisualParityArtifacts,
  runStaticVisualParity,
  writeStaticVisualParitySummary,
};
