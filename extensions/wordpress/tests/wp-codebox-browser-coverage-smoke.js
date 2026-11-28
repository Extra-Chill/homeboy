'use strict';

const assert = require('node:assert/strict');
const { existsSync } = require('node:fs');
const { mkdtemp, mkdir, readFile, rm, writeFile } = require('node:fs/promises');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { getWordPressHelperManifest } = require('../lib/helper-manifest');

const { runWpCodeboxBrowserCoverageTrace } = require(getWordPressHelperManifest().helpers.wpCodeboxBrowserCoverage);

async function runLifecycleSmoke() {
  const workspace = await mkdtemp(path.join(tmpdir(), 'wp-codebox-browser-coverage-smoke.'));
  const stepsFile = path.join(workspace, 'steps.json');
  const resultsFile = path.join(workspace, 'results', 'trace.json');
  const artifactDir = path.join(workspace, 'artifacts');
  const componentPath = path.join(workspace, 'component');

  for (const name of ['HOMEBOY_COMPONENT_ID', 'HOMEBOY_TRACE_SCENARIO', 'HOMEBOY_TRACE_RESULTS_FILE', 'HOMEBOY_TRACE_ARTIFACT_DIR']) {
    delete process.env[name];
  }

  try {
    await mkdir(componentPath, { recursive: true });
    await writeFile(path.join(componentPath, 'plugin.php'), '<?php');
    const steps = [{ kind: 'navigate', url: '/' }];
    await writeFile(stepsFile, JSON.stringify(steps));
    const traceResult = await runWpCodeboxBrowserCoverageTrace({
      componentId: 'sample-plugin',
      scenarioId: 'sample-browser-coverage',
      resultsFile,
      artifactDir,
      componentPath,
      requiredFile: 'plugin.php',
      setupCode: '<?php update_option( "homeboy_smoke", "1" );',
      assumptions: ['fake recipe runner produces browser artifacts'],
      scenarios: [{ id: 'front', stepsFile }],
      runRecipe: async ({ recipeFile, artifactsDir, outputFile }) => {
        const recipe = JSON.parse(await readFile(recipeFile, 'utf8'));
        assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
        assert.equal(recipe.workflow.steps.length, 2);
        const browserStep = recipe.workflow.steps[1];
        assert.equal(browserStep.command, 'wordpress.browser-actions');
        const combinedStepsFile = browserStep.args.find((arg) => arg.startsWith('steps-json=@')).slice('steps-json=@'.length);
        assert.deepEqual(JSON.parse(await readFile(combinedStepsFile, 'utf8')), steps);

        const browserDir = path.join(artifactsDir, 'files', 'browser');
        await mkdir(browserDir, { recursive: true });
        await writeFile(path.join(browserDir, 'action-summary.json'), `${JSON.stringify({ finalUrl: 'https://example.test/' })}\n`);
        await writeFile(path.join(browserDir, 'request-coverage.json'), `${JSON.stringify({ schema: 'homeboy/browser-request-coverage/v1' })}\n`);
        await writeFile(path.join(browserDir, 'network.jsonl'), `${JSON.stringify({ type: 'request' })}\n${JSON.stringify({ type: 'response' })}\n`);
        await writeFile(path.join(browserDir, 'errors.jsonl'), '');
        await writeFile(path.join(browserDir, 'steps.jsonl'), `${JSON.stringify({ status: 'ok' })}\n`);

        const output = { artifacts: { directory: artifactsDir } };
        await writeFile(outputFile, `${JSON.stringify(output)}\n`);
        return { stdout: JSON.stringify(output), json: output };
      },
    });

    const writtenResult = JSON.parse(await readFile(resultsFile, 'utf8'));
    assert.equal(traceResult.status, 'pass');
    assert.equal(writtenResult.status, 'pass');
    assert.equal(writtenResult.component_id, 'sample-plugin');
    assert.equal(writtenResult.scenario_id, 'sample-browser-coverage');
    assert.match(writtenResult.summary, /1 scenario\(s\), 2 network event\(s\), 1 response/);
    assert.deepEqual(writtenResult.assertions.map((item) => item.status), ['pass', 'pass', 'pass']);
    assert.equal(writtenResult.metadata.final_url, 'https://example.test/');
    assert.equal(writtenResult.metadata.request_coverage_schema, 'homeboy/browser-request-coverage/v1');
    assert.ok(writtenResult.artifacts.some((artifact) => artifact.path.endsWith('request-coverage.json')));
    assert.ok(existsSync(path.join(artifactDir, 'wp-codebox-output.json')));
  } finally {
    await rm(workspace, { recursive: true, force: true });
  }
}

runLifecycleSmoke().then(() => {
  console.log('WP Codebox browser coverage trace smoke passed.');
});
