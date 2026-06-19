import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import { join } from 'node:path';

import { runWordPressCodeboxVisualParityWorkload } from '../lib/wordpress-codebox-visual-parity-workload.mjs';

const root = await mkdtemp(join(os.tmpdir(), 'wordpress-codebox-visual-parity-workload-'));

try {
  const componentPath = join(root, 'component-under-test');
  const sourceDir = join(componentPath, 'source-site');
  await mkdir(sourceDir, { recursive: true });
  await writeFile(join(sourceDir, 'index.html'), '<main>Source</main>');
  process.env.HOMEBOY_COMPONENT_PATH = componentPath;

  const fakeCli = join(componentPath, 'fake-wp-codebox-cli.mjs');
  await writeFile(fakeCli, `#!/usr/bin/env node
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const recipePath = process.argv[process.argv.indexOf('--recipe') + 1];
const recipe = JSON.parse(await readFile(recipePath, 'utf8'));
const compareStep = recipe.workflow.steps.find((step) => step.command === 'wordpress.visual-compare');
const args = compareStep.args;
const arg = (name) => args.find((item) => item.startsWith(name + '='))?.slice(name.length + 1);
const sourceResponse = await fetch(arg('source-url'));
if (!sourceResponse.ok) throw new Error('source URL was not served');
const dir = join(recipe.artifacts.directory, 'files/browser/visual-compare');
await mkdir(dir, { recursive: true });
await writeFile(join(dir, 'source.png'), 'source');
await writeFile(join(dir, 'candidate.png'), 'candidate');
await writeFile(join(dir, 'diff.png'), 'diff');
const visual = {
  schema: 'wp-codebox/visual-compare/v1',
  status: 'different',
  files: {
    sourceScreenshot: 'files/browser/visual-compare/source.png',
    candidateScreenshot: 'files/browser/visual-compare/candidate.png',
    diffScreenshot: 'files/browser/visual-compare/diff.png',
    visualDiff: 'files/browser/visual-compare/visual-diff.json',
    summary: 'files/browser/visual-compare/summary.json'
  },
  viewport: { width: 640, height: 480 },
  comparison: { mismatchPixels: 10, totalPixels: 1000, mismatchRatio: 0.01, dimensionMismatch: false, regions: [{ x: 1, y: 2, width: 3, height: 4 }] }
};
await writeFile(join(dir, 'visual-diff.json'), JSON.stringify(visual, null, 2));
await writeFile(join(dir, 'summary.json'), JSON.stringify(visual, null, 2));
process.stdout.write(JSON.stringify({
  success: true,
  commands: [{ artifact: { files: { visualDiff: 'files/browser/visual-compare/visual-diff.json' } } }]
}));
`);
  await chmod(fakeCli, 0o755);

  const visualResult = await runWordPressCodeboxVisualParityWorkload({
    id: 'Visual Contract',
    artifactsDir: join(componentPath, 'visual-artifacts'),
    runId: 'visual-run',
    backend: { codeboxCli: fakeCli },
    source: { path: sourceDir, ref: 'source-ref', label: 'static-source', port: 48531 },
    candidate: {
      url: '/',
      ref: 'candidate-ref',
      label: 'candidate-wordpress',
      context: { runtime: 'playground' },
      recipe: { runtime: { wp: 'latest' }, inputs: { mounts: [] }, workflow: { steps: [{ command: 'wordpress.setup', args: [] }] } },
    },
    viewport: { width: 640, height: 480 },
    threshold: 0.02,
  });
  assert.equal(visualResult.metrics.visual_parity_pass, 1);
  assert.equal(visualResult.metrics.visual_parity_mismatch_ratio, 0.01);
  const visualArtifact = JSON.parse(await readFile(visualResult.artifacts.visualParity.path, 'utf8'));
  assert.equal(visualArtifact.schema, 'homeboy/VisualParityArtifact/v1');
  assert.equal(visualArtifact.source.ref, 'source-ref');
  assert.equal(visualArtifact.candidate.ref, 'candidate-ref');
  assert.equal(visualArtifact.summary.status, 'passed');
  assert.equal(visualArtifact.summary.region_count, 1);
  assert.equal(visualArtifact.artifacts.visual_diff, 'files/browser/visual-compare/visual-diff.json');
  const recipeArtifact = JSON.parse(await readFile(visualResult.metadata.codebox_recipe, 'utf8'));
  assert.equal(recipeArtifact.runtime.wp, 'latest');
  assert.equal(recipeArtifact.workflow.steps[0].command, 'wordpress.setup');
  assert.equal(recipeArtifact.workflow.steps[1].command, 'wordpress.visual-compare');
  assert.equal(recipeArtifact.workflow.steps[1].args.includes('viewport=640x480'), true);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WordPress Codebox visual parity workload smoke passed.');
