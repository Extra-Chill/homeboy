#!/usr/bin/env node
/**
 * External dependencies
 */
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const packageRoot = path.dirname(fileURLToPath(import.meta.url));
const supportedPhpVersions = new Set(['7.4', '8.0', '8.1', '8.2', '8.3', '8.4', '8.5']);

export async function buildRecipe(settings = {}, cwd = process.cwd()) {
  const fixturePlugin = path.join(packageRoot, 'fixtures/network-fixture');
  const phpVersion = runtimePhpVersion(settings);
  const themes = await extraThemes(settings.wp_codebox_extra_themes);
  const activeTheme = themes.find((theme) => theme.activate);
  const blueprint = mergeMultisiteBlueprint(settings.wordpress_runtime_blueprint, activeTheme?.slug);
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
      ...(phpVersion ? { phpVersion } : {}),
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
      mounts: themes.map((theme) => ({
        type: 'directory',
        source: theme.source,
        target: `/wordpress/wp-content/themes/${theme.slug}`,
        mode: 'readonly',
        metadata: {
          ...theme.metadata,
          kind: 'wordpress-theme',
          slug: theme.slug,
        },
      })),
    },
    workflow: {
      steps: [
        phpFileStep('network-seed.php'),
        ...(activeTheme ? [activateThemeStep(activeTheme.slug)] : []),
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

function mergeMultisiteBlueprint(value, activeThemeSlug) {
  const blueprint = value && typeof value === 'object' && !Array.isArray(value) ? value : {};
  const steps = Array.isArray(blueprint.steps) ? [...blueprint.steps] : [];
  if (activeThemeSlug) {
    const defineIndex = steps.findIndex((step) => step?.step === 'defineWpConfigConsts');
    if (defineIndex >= 0) {
      const existing = steps[defineIndex]?.consts?.WP_DEFAULT_THEME;
      if (existing !== undefined && existing !== activeThemeSlug) {
        throw new Error('wordpress_runtime_blueprint WP_DEFAULT_THEME must match the active wp_codebox_extra_themes slug.');
      }
      steps[defineIndex] = {
        ...steps[defineIndex],
        consts: { ...(steps[defineIndex].consts || {}), WP_DEFAULT_THEME: activeThemeSlug },
      };
    } else {
      steps.push({ step: 'defineWpConfigConsts', consts: { WP_DEFAULT_THEME: activeThemeSlug } });
    }
  }
  return {
    ...blueprint,
    steps: steps.some((step) => step?.step === 'enableMultisite') ? steps : [{ step: 'enableMultisite' }, ...steps],
  };
}

function runtimePhpVersion(settings) {
  if (!Object.hasOwn(settings, 'wordpress_runtime_php_version')) {
    return undefined;
  }
  const value = settings.wordpress_runtime_php_version;
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error('wordpress_runtime_php_version must be a non-empty PHP major.minor version.');
  }
  const version = value.trim();
  if (!supportedPhpVersions.has(version)) {
    throw new Error(`Unsupported wordpress_runtime_php_version: ${version}.`);
  }
  return version;
}

async function extraThemes(value) {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new Error('wp_codebox_extra_themes must be an array.');
  }

  const themes = [];
  const slugs = new Set();
  for (const [index, entry] of value.entries()) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      throw new Error(`wp_codebox_extra_themes[${index}] must be an object.`);
    }
    if (typeof entry.source !== 'string' || !path.isAbsolute(entry.source)) {
      throw new Error(`wp_codebox_extra_themes[${index}].source must be an absolute path.`);
    }
    const source = path.resolve(entry.source);
    let sourceStat;
    try {
      sourceStat = await stat(source);
    } catch {
      throw new Error(`wp_codebox_extra_themes[${index}].source does not exist: ${source}.`);
    }
    if (!sourceStat.isDirectory()) {
      throw new Error(`wp_codebox_extra_themes[${index}].source must be a directory: ${source}.`);
    }
    if (typeof entry.slug !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(entry.slug)) {
      throw new Error(`wp_codebox_extra_themes[${index}].slug must be a valid WordPress theme directory slug.`);
    }
    if (slugs.has(entry.slug)) {
      throw new Error(`wp_codebox_extra_themes contains duplicate slug: ${entry.slug}.`);
    }
    slugs.add(entry.slug);
    if (entry.activate !== undefined && typeof entry.activate !== 'boolean') {
      throw new Error(`wp_codebox_extra_themes[${index}].activate must be a boolean.`);
    }
    if (entry.metadata !== undefined && (!entry.metadata || typeof entry.metadata !== 'object' || Array.isArray(entry.metadata))) {
      throw new Error(`wp_codebox_extra_themes[${index}].metadata must be an object.`);
    }

    let style;
    try {
      style = await readFile(path.join(source, 'style.css'), 'utf8');
    } catch {
      throw new Error(`wp_codebox_extra_themes[${index}] must contain style.css.`);
    }
    if (!/^[ \t*]*Theme Name\s*:/mi.test(style)) {
      throw new Error(`wp_codebox_extra_themes[${index}].style.css must contain a Theme Name header.`);
    }

    themes.push({
      source,
      slug: entry.slug,
      activate: entry.activate === true,
      metadata: entry.metadata || {},
    });
  }

  if (themes.filter((theme) => theme.activate).length > 1) {
    throw new Error('wp_codebox_extra_themes permits at most one active theme.');
  }
  return themes;
}

function activateThemeStep(slug) {
  const code = `$theme_slug = '${slug}';
if ( ! is_multisite() ) {
\tthrow new RuntimeException( 'Expected a multisite runtime.' );
}
foreach ( get_sites( array( 'number' => 0, 'fields' => 'ids' ) ) as $site_id ) {
\tswitch_to_blog( (int) $site_id );
\ttry {
\t\t$theme = wp_get_theme( $theme_slug );
\t\tif ( ! $theme->exists() ) {
\t\t\tthrow new RuntimeException( 'Mounted theme is unavailable: ' . $theme_slug );
\t\t}
\t\tswitch_theme( $theme_slug );
\t\tif ( get_stylesheet() !== $theme_slug ) {
\t\t\tthrow new RuntimeException( 'Unable to activate mounted theme: ' . $theme_slug );
\t\t}
\t} finally {
\t\trestore_current_blog();
\t}
}`;
  return {
    command: 'wordpress.run-php',
    args: [`code=${code}`],
    metadata: { kind: 'wordpress-theme-activation', slug },
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
