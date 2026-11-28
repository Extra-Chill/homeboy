'use strict';

/**
 * External dependencies
 */
const { existsSync } = require('node:fs');
const { mkdir, mkdtemp, readFile, rm, writeFile } = require('node:fs/promises');
const { tmpdir } = require('node:os');
const path = require('node:path');
const { performance } = require('node:perf_hooks');

/**
 * Internal dependencies
 */
const { wpCodeboxBrowserArtifacts } = require('./wp-codebox-artifacts');
const { runWpCodeboxRecipe } = require('./wp-codebox-recipe-helper');

// Adapt Codebox browser-action artifacts to the rig trace result contract.
async function runWpCodeboxBrowserCoverageTrace(config = {}) {
  const componentId = process.env.HOMEBOY_COMPONENT_ID || config.componentId || config.component_id;
  const scenarioId = process.env.HOMEBOY_TRACE_SCENARIO || config.scenarioId || config.scenario_id;
  const resultsFile = process.env.HOMEBOY_TRACE_RESULTS_FILE || config.resultsFile || config.results_file;
  const artifactDir = process.env.HOMEBOY_TRACE_ARTIFACT_DIR
    || config.artifactDir
    || config.artifact_dir
    || path.join(tmpdir(), `${scenarioId || 'wp-codebox-browser-coverage'}-artifacts`);
  const componentPath = config.componentPath || config.component_path || process.env.HOMEBOY_COMPONENT_PATH;
  const wpVersion = config.wpVersion || config.wp_version || '7.0';
  const viewport = config.viewport || '1366x900';
  const stepTimeout = config.stepTimeout || config.step_timeout || '45s';
  const timeout = config.timeout || '180s';
  const runRecipe = config.runRecipe || runWpCodeboxRecipe;
  const startedAt = performance.now();
  const timeline = [];

  if (!resultsFile) {
    throw new Error('HOMEBOY_TRACE_RESULTS_FILE is required');
  }
  if (config.requiredFile && !existsSync(path.join(componentPath || '', config.requiredFile))) {
    throw new Error(`Missing required component file at ${path.join(componentPath || '', config.requiredFile)}`);
  }

  await mkdir(artifactDir, { recursive: true });
  await mkdir(path.dirname(resultsFile), { recursive: true });

  const workDir = await mkdtemp(path.join(tmpdir(), `${scenarioId || 'wp-codebox-browser-coverage'}.`));
  const setupFile = path.join(workDir, 'setup.php');
  const combinedStepsFile = path.join(workDir, 'browser-actions.json');
  const recipeFile = path.join(workDir, 'recipe.json');
  const outputFile = path.join(artifactDir, 'wp-codebox-output.json');
  const codeboxArtifacts = path.join(artifactDir, 'wp-codebox-artifacts');

  function timestampMs() {
    return Math.round(performance.now() - startedAt);
  }

  function event(source, name, data = {}) {
    timeline.push({ t_ms: timestampMs(), source, event: name, data });
  }

  function relativeArtifactPath(pathname) {
    return path.relative(artifactDir, pathname);
  }

  async function readJsonAsync(pathname) {
    return existsSync(pathname) ? JSON.parse(await readFile(pathname, 'utf8')) : null;
  }

  async function readJsonl(pathname) {
    if (!existsSync(pathname)) {
      return [];
    }

    const contents = await readFile(pathname, 'utf8');
    return contents.trim().split('\n').filter(Boolean).map((line) => JSON.parse(line));
  }

  try {
    const scenarios = Array.isArray(config.scenarios) ? config.scenarios : [];
    event('scenario', 'start', {
      component_path: componentPath,
      wp_version: wpVersion,
      scenarios: scenarios.map((scenario) => scenario.id),
    });

    const workflowSteps = [];
    if (config.setupCode || config.setup_code) {
      await writeFile(setupFile, config.setupCode || config.setup_code);
      workflowSteps.push({ command: 'wordpress.run-php', args: [`code-file=${setupFile}`] });
    }

    const combinedSteps = [];
    for (const scenario of scenarios) {
      const stepsFile = scenario.stepsFile || scenario.steps_file;
      const steps = JSON.parse(await readFile(stepsFile, 'utf8'));
      if (!Array.isArray(steps)) {
        throw new Error(`Browser scenario steps must be an array: ${stepsFile}`);
      }
      combinedSteps.push(...steps);
    }
    await writeFile(combinedStepsFile, `${JSON.stringify(combinedSteps, null, 2)}\n`);
    workflowSteps.push({
      command: 'wordpress.browser-actions',
      args: [
        `step-timeout=${stepTimeout}`,
        `timeout=${timeout}`,
        `viewport=${viewport}`,
        'capture=steps,console,errors,network,html,screenshot,dom-snapshot',
        `steps-json=@${combinedStepsFile}`,
      ],
    });

    const recipe = {
      schema: 'wp-codebox/workspace-recipe/v1',
      runtime: {
        wp: wpVersion,
        blueprint: {
          steps: config.blueprintSteps || config.blueprint_steps || [{ step: 'login', username: 'admin', password: 'password' }],
        },
      },
      ...((config.inputs) ? { inputs: config.inputs } : {}),
      workflow: { steps: workflowSteps },
      artifacts: { directory: codeboxArtifacts },
    };

    await writeFile(recipeFile, `${JSON.stringify(recipe, null, 2)}\n`);

    const result = await runRecipe({
      recipeFile,
      artifactsDir: codeboxArtifacts,
      outputFile,
      event,
      maxBuffer: 1024 * 1024 * 50,
    });

    const output = result.json || JSON.parse(result.stdout);
    const browserArtifacts = wpCodeboxBrowserArtifacts(output, [
      'action-summary.json',
      'network.jsonl',
      'request-coverage.json',
      'errors.jsonl',
      'steps.jsonl',
    ]);
    const summaryPath = browserArtifacts['action-summary.json'];
    const networkPath = browserArtifacts['network.jsonl'];
    const requestCoveragePath = browserArtifacts['request-coverage.json'];
    const errorsPath = browserArtifacts['errors.jsonl'];
    const stepsPath = browserArtifacts['steps.jsonl'];
    const summary = await readJsonAsync(summaryPath);
    const requestCoverage = await readJsonAsync(requestCoveragePath);
    const network = await readJsonl(networkPath);
    const errors = await readJsonl(errorsPath);
    const steps = await readJsonl(stepsPath);
    const failedSteps = steps.filter((step) => step.status === 'failed');
    const responseCount = network.filter((entry) => entry.type === 'response').length;
    const requestCoverageReady = Boolean(requestCoverage && existsSync(requestCoveragePath));
    const pass = requestCoverageReady && failedSteps.length === 0;

    event('browser', 'actions.ready', {
      request_coverage_ready: requestCoverageReady,
      network_events: network.length,
      responses: responseCount,
      failed_steps: failedSteps.length,
    });

    const traceResult = {
      component_id: componentId,
      scenario_id: scenarioId,
      status: pass ? 'pass' : 'fail',
      summary: requestCoverageReady
        ? `Captured WP Codebox browser request coverage for ${scenarios.length} scenario(s), ${network.length} network event(s), ${responseCount} response(s).`
        : 'WP Codebox browser request coverage artifact was not produced.',
      timeline,
      assertions: [
        {
          id: 'browser-request-coverage-produced',
          status: requestCoverageReady ? 'pass' : 'fail',
          message: requestCoverageReady ? 'request-coverage.json was produced by wordpress.browser-actions.' : 'request-coverage.json was missing.',
        },
        {
          id: 'browser-actions-completed',
          status: failedSteps.length === 0 ? 'pass' : 'fail',
          message: `Recorded ${failedSteps.length} failed browser action step(s).`,
        },
        {
          id: 'browser-errors-recorded',
          status: 'pass',
          message: `Recorded ${errors.length} browser/runtime error artifact entr${errors.length === 1 ? 'y' : 'ies'}.`,
        },
      ],
      artifacts: [
        { label: 'WP Codebox output', path: relativeArtifactPath(outputFile) },
        ...(summaryPath && existsSync(summaryPath) ? [{ label: 'Browser actions summary', path: relativeArtifactPath(summaryPath) }] : []),
        ...(requestCoveragePath && existsSync(requestCoveragePath) ? [{ label: 'Browser request coverage', path: relativeArtifactPath(requestCoveragePath) }] : []),
        ...(networkPath && existsSync(networkPath) ? [{ label: 'Browser network log', path: relativeArtifactPath(networkPath) }] : []),
      ],
      metadata: {
        assumptions: config.assumptions || [],
        final_url: summary?.finalUrl || summary?.summary?.finalUrl || null,
        request_coverage_schema: requestCoverage?.schema || null,
      },
    };

    await writeFile(resultsFile, `${JSON.stringify(traceResult, null, 2)}\n`);
    process.exitCode = pass ? 0 : 1;
    return traceResult;
  } catch (error) {
    const traceResult = {
      component_id: componentId,
      scenario_id: scenarioId,
      status: 'fail',
      summary: error instanceof Error ? error.message : String(error),
      timeline,
      assertions: [
        {
          id: 'trace-workload-completed',
          status: 'fail',
          message: error instanceof Error ? error.message : String(error),
        },
      ],
      artifacts: existsSync(outputFile) ? [{ label: 'WP Codebox output', path: relativeArtifactPath(outputFile) }] : [],
    };
    await writeFile(resultsFile, `${JSON.stringify(traceResult, null, 2)}\n`);
    throw error;
  } finally {
    await rm(workDir, { recursive: true, force: true });
  }
}

module.exports = { runWpCodeboxBrowserCoverageTrace };
