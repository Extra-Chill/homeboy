#!/usr/bin/env node
/**
 * External dependencies
 */
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const packageRoot = path.dirname(fileURLToPath(import.meta.url));

export async function buildRecipe(settings = {}, cwd = process.cwd()) {
  const fixturePlugin = path.join(packageRoot, 'fixtures/network-fixture');
  const blueprint = mergeMultisiteBlueprint(settings.wordpress_runtime_blueprint);
  const prepareSteps = recipeSteps(settings.wordpress_runtime_prepare_steps);
  const postSteps = recipeSteps(settings.wordpress_runtime_post_steps);
  const scenarioSteps = await browserScenarioSteps(settings.wp_codebox_scenario_manifests, cwd);
  const workloadSteps = settings.wordpress_runtime_workloads?.length ? [{
    command: 'wordpress.bench',
    args: [
      'component-id=wordpress-multisite-e2e',
      'plugin-slug=synthetic-network-fixture',
      'iterations=1',
      'warmup=0',
      `workloads-json=${JSON.stringify(settings.wordpress_runtime_workloads)}`,
    ],
  }] : [];

  return {
    schema: 'wp-codebox/workspace-recipe/v1',
    runtime: {
      backend: 'wordpress',
      ...(settings.wordpress_runtime_version ? { wp: settings.wordpress_runtime_version } : {}),
      blueprint,
      preview: { siteUrl: 'http://localhost' },
    },
    inputs: {
      extra_plugins: [
        {
          source: fixturePlugin,
          slug: 'synthetic-network-fixture',
          pluginFile: 'synthetic-network-fixture/network-fixture.php',
          activate: false,
        },
        ...(settings.wp_codebox_extra_plugins || []),
      ],
    },
    workflow: {
      steps: [
        phpFileStep('network-seed.php'),
        ...prepareSteps,
        ...workloadSteps,
        phpFileStep('network-assert.php'),
        {
          command: 'wordpress.browser-probe',
          args: [
            'url=http://localhost/alpha/fixture-check/',
            'route-host=localhost',
            'assert=exists:#synthetic-site-alpha',
            'assert=exists:#synthetic-auth-anonymous',
            'assert=no-console-errors',
            'assert=no-page-errors',
            'capture=console,errors,html,network,screenshot',
          ],
        },
        {
          command: 'wordpress.browser-actions',
          args: [
            'auth=wordpress-admin',
            'auth-user-id=1',
            `steps-json=${JSON.stringify([
              { kind: 'navigate', url: '/alpha/fixture-check/', waitFor: 'load' },
              { kind: 'expect', selector: '#synthetic-site-alpha', state: 'visible' },
              { kind: 'expect', selector: '#synthetic-auth-authenticated', state: 'visible' },
              { kind: 'navigate', url: '/beta/fixture-check/', waitFor: 'load' },
              { kind: 'expect', selector: '#synthetic-site-beta', state: 'visible' },
              { kind: 'expect', selector: '#synthetic-auth-authenticated', state: 'visible' },
            ])}`,
            'capture=steps,console,errors,html,network,screenshot,dom-snapshot',
          ],
        },
        ...scenarioSteps,
        ...postSteps,
      ],
    },
    artifacts: {
      directory: process.env.HOMEBOY_ARTIFACT_ROOT || path.join(cwd, 'artifacts/wordpress-multisite-e2e'),
    },
  };
}

function mergeMultisiteBlueprint(value) {
  const blueprint = value && typeof value === 'object' && !Array.isArray(value) ? value : {};
  const steps = Array.isArray(blueprint.steps) ? blueprint.steps : [];
  return {
    ...blueprint,
    steps: steps.some((step) => step?.step === 'enableMultisite') ? steps : [{ step: 'enableMultisite' }, ...steps],
  };
}

function phpFileStep(file) {
  return {
    command: 'wordpress.run-php',
    args: [`code-file=${path.join(packageRoot, 'fixtures', file)}`],
  };
}

function recipeSteps(value) {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new Error('WordPress runtime recipe steps must be arrays.');
  }
  return value;
}

async function browserScenarioSteps(value, cwd) {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new Error('wp_codebox_scenario_manifests must be an array.');
  }
  const scenarios = [];
  for (const entry of value) {
    const scenario = typeof entry === 'string'
      ? JSON.parse(await readFile(path.resolve(cwd, entry), 'utf8'))
      : entry;
    scenarios.push({ command: 'wordpress.browser-scenario', args: [`scenario-json=${JSON.stringify(scenario)}`] });
  }
  return scenarios;
}

async function main() {
  const dryRun = process.argv.includes('--dry-run');
  const settings = parseSettings(process.env.HOMEBOY_SETTINGS_JSON);
  const recipe = await buildRecipe(settings);
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'wordpress-multisite-e2e-'));
  const recipePath = path.join(temporary, 'recipe.json');
  const resultPath = process.env.HOMEBOY_NETWORK_E2E_RESULT_FILE || path.join(temporary, 'result.json');
  await writeFile(recipePath, `${JSON.stringify(recipe, null, 2)}\n`);

  try {
    runCodebox(['recipe', 'validate', '--recipe', recipePath, '--json']);
    const args = ['recipe-run', '--recipe', recipePath, '--artifacts', recipe.artifacts.directory, '--json'];
    if (dryRun) {
      args.push('--dry-run');
    }
    const result = runCodebox(args, true);
    await mkdir(path.dirname(resultPath), { recursive: true });
    await writeFile(resultPath, result.stdout);
    const envelope = JSON.parse(result.stdout);
    if (!dryRun && envelope.success !== true) {
      throw new Error('WP Codebox multisite recipe did not succeed.');
    }
    process.stdout.write(result.stdout);
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

function runCodebox(args, capture = false) {
  const executable = process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.WP_CODEBOX_BIN || 'wp-codebox';
  const result = spawnSync(executable, args, { encoding: 'utf8', stdio: capture ? 'pipe' : 'inherit' });
  if (result.error) {
    throw result.error;
  }
  if (capture && result.stderr) {
    process.stderr.write(result.stderr);
  }
  if (result.status !== 0) {
    throw new Error(`WP Codebox exited with status ${result.status}.`);
  }
  return result;
}

function parseSettings(raw) {
  if (!raw) {
    return {};
  }
  const value = JSON.parse(raw);
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('HOMEBOY_SETTINGS_JSON must contain a JSON object.');
  }
  return value;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main();
}
