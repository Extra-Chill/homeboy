/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
/**
 * Internal dependencies
 */
import { buildRecipe, runCodebox } from '../run.mjs';

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const temporary = await mkdtemp(path.join(os.tmpdir(), 'wordpress-multisite-e2e-contract-'));
const theme = path.join(temporary, 'consumer-theme');
const secondTheme = path.join(temporary, 'second-theme');
const noHeaderTheme = path.join(temporary, 'no-header-theme');
const noStyleTheme = path.join(temporary, 'no-style-theme');
const emptyHeaderTheme = path.join(temporary, 'empty-header-theme');
const lateHeaderTheme = path.join(temporary, 'late-header-theme');
const noEntrypointTheme = path.join(temporary, 'no-entrypoint-theme');
const childTheme = path.join(temporary, 'child-theme');
const notDirectory = path.join(temporary, 'not-directory');

try {
  await Promise.all([
    mkdir(theme),
    mkdir(path.join(secondTheme, 'templates'), { recursive: true }),
    mkdir(noHeaderTheme),
    mkdir(noStyleTheme),
    mkdir(emptyHeaderTheme),
    mkdir(lateHeaderTheme),
    mkdir(noEntrypointTheme),
    mkdir(childTheme),
  ]);
  await Promise.all([
    writeFile(path.join(theme, 'style.css'), '/*\nTheme Name: Consumer Theme\n*/\n'),
    writeFile(path.join(theme, 'index.php'), '<?php\n'),
    writeFile(path.join(secondTheme, 'style.css'), '/*\n * Theme Name: Second Theme\n */\n'),
    writeFile(path.join(secondTheme, 'templates/index.html'), '<!-- wp:paragraph --><p>Second theme</p><!-- /wp:paragraph -->\n'),
    writeFile(path.join(noHeaderTheme, 'style.css'), '/* No theme header. */\n'),
    writeFile(path.join(noHeaderTheme, 'index.php'), '<?php\n'),
    writeFile(path.join(emptyHeaderTheme, 'style.css'), '/*\nTheme Name:   \n*/\n'),
    writeFile(path.join(emptyHeaderTheme, 'index.php'), '<?php\n'),
    writeFile(path.join(lateHeaderTheme, 'style.css'), `${'x'.repeat(8192)}\nTheme Name: Too Late\n`),
    writeFile(path.join(lateHeaderTheme, 'index.php'), '<?php\n'),
    writeFile(path.join(noEntrypointTheme, 'style.css'), '/*\nTheme Name: No Entrypoint\n*/\n'),
    writeFile(path.join(childTheme, 'style.css'), '/*\nTheme Name: Child Theme\nTemplate: consumer-theme\n*/\n'),
    writeFile(notDirectory, 'not a theme directory\n'),
  ]);

  const prepareStep = { command: 'wordpress.wp-cli', args: ['command=site list'] };
  const recipe = await buildRecipe({
    wordpress_runtime_version: 'nightly',
    wordpress_runtime_php_version: '8.4',
    wordpress_runtime_blueprint: { steps: [{ step: 'setSiteOptions', options: { blogname: 'Fixture Network' } }] },
    wp_codebox_extra_plugins: [{ source: '/tmp/consumer-plugin', slug: 'consumer-plugin', activate: false }],
    wp_codebox_extra_themes: [{
      source: theme,
      slug: 'consumer-theme',
      activate: true,
      metadata: { provenance: { revision: '0123456789abcdef' } },
    }],
    wp_codebox_dependency_overlays: [{
      kind: 'composer-package',
      package: 'example/runtime-package',
      source: '/tmp/runtime-package',
      consumer: 'consumer-plugin',
      metadata: { provenance: { revision: 'fedcba9876543210' } },
    }],
    wordpress_runtime_prepare_steps: [prepareStep],
    wordpress_runtime_workloads: [{ id: 'consumer-workload', run: [] }],
    wp_codebox_scenario_manifests: [{ id: 'consumer-browser-journey', url: '/beta/' }],
    wordpress_runtime_post_steps: [{ command: 'wordpress.wp-cli', args: ['command=option get home'] }],
  }, root);

  assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
  assert.equal(recipe.runtime.wp, 'nightly');
  assert.equal(recipe.runtime.phpVersion, '8.4');
  assert.equal(recipe.runtime.preview.siteUrl, 'http://localhost');
  assert.equal(recipe.runtime.blueprint.steps[0].step, 'enableMultisite');
  const defaultThemeStep = recipe.runtime.blueprint.steps.find((step) => step.step === 'defineWpConfigConsts');
  assert.equal(defaultThemeStep.consts.WP_DEFAULT_THEME, 'consumer-theme');
  assert.equal(recipe.inputs.extra_plugins[0].pluginFile, 'synthetic-network-fixture/network-fixture.php');
  assert.equal(recipe.inputs.extra_plugins[0].activate, false);
  assert.equal(recipe.inputs.extra_plugins[1].slug, 'consumer-plugin');
  assert.deepEqual(recipe.inputs.mounts, [{
    type: 'directory',
    source: theme,
    target: '/wordpress/wp-content/themes/consumer-theme',
    mode: 'readonly',
    metadata: {
      provenance: { revision: '0123456789abcdef' },
      kind: 'wordpress-theme',
      slug: 'consumer-theme',
    },
  }]);
  assert.deepEqual(recipe.inputs.dependency_overlays, [{
    kind: 'composer-package',
    package: 'example/runtime-package',
    source: '/tmp/runtime-package',
    consumer: 'consumer-plugin',
    metadata: { provenance: { revision: 'fedcba9876543210' } },
  }]);
  assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.bench'));
  const browserSteps = recipe.workflow.steps.filter((step) => ['wordpress.browser-probe', 'wordpress.browser-actions', 'wordpress.browser-scenario'].includes(step.command));
  assert.ok(browserSteps.some((step) => step.command === 'wordpress.browser-scenario'));
  for (const step of browserSteps) {
    assert.ok(step.args.includes('route-host=localhost'));
    assert.ok(step.args.includes('network-policy=block'));
    assert.ok(step.args.includes('allow-host=localhost'));
  }
  assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.browser-probe'));
  assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.browser-actions'));
  const seedIndex = recipe.workflow.steps.findIndex((step) => step.args?.some((arg) => arg.includes('network-seed.php')));
  const activationIndex = recipe.workflow.steps.findIndex((step) => step.metadata?.kind === 'wordpress-theme-activation');
  const prepareIndex = recipe.workflow.steps.indexOf(prepareStep);
  assert.ok(seedIndex >= 0 && seedIndex < activationIndex && activationIndex < prepareIndex);
  assert.ok(recipe.workflow.steps[activationIndex].args[0].includes("switch_theme( $theme_slug )"));
  assert.ok(recipe.workflow.steps.some((step) => step.args?.some((arg) => arg.includes('network-assert.php'))));

  const childRecipe = await buildRecipe({ wp_codebox_extra_themes: [
    { source: theme, slug: 'consumer-theme' },
    { source: childTheme, slug: 'child-theme', activate: true },
  ] }, root);
  assert.equal(childRecipe.inputs.mounts.length, 2);
  assert.equal(childRecipe.runtime.blueprint.steps.find((step) => step.step === 'defineWpConfigConsts').consts.WP_DEFAULT_THEME, 'child-theme');

  const repeatedDefinitions = await buildRecipe({
    wordpress_runtime_blueprint: { steps: [
      { step: 'defineWpConfigConsts', consts: { WP_DEBUG: true } },
      { step: 'defineWpConfigConsts', consts: { WP_DEFAULT_THEME: 'consumer-theme' } },
    ] },
    wp_codebox_extra_themes: [{ source: theme, slug: 'consumer-theme', activate: true }],
  }, root);
  assert.equal(repeatedDefinitions.runtime.blueprint.steps[1].consts.WP_DEFAULT_THEME, 'consumer-theme');
  assert.equal(repeatedDefinitions.runtime.blueprint.steps[2].consts.WP_DEFAULT_THEME, 'consumer-theme');

  const withoutRuntimeInputs = await buildRecipe({}, root);
  assert.equal(Object.hasOwn(withoutRuntimeInputs.runtime, 'phpVersion'), false);
  assert.deepEqual(withoutRuntimeInputs.inputs.mounts, []);
  assert.deepEqual(withoutRuntimeInputs.inputs.dependency_overlays, []);
  assert.equal(withoutRuntimeInputs.workflow.steps.some((step) => step.metadata?.kind === 'wordpress-theme-activation'), false);

  const consumerTopology = await buildRecipe({
    wordpress_multisite_synthetic_fixture: false,
    wp_codebox_extra_plugins: [{ source: '/tmp/consumer-plugin', slug: 'consumer-plugin', activate: false }],
    wordpress_runtime_workload_plugin_slug: 'consumer-plugin',
    wordpress_runtime_workloads: [{ id: 'file-workload', run: [{ type: 'php', file: '/tmp/workload.php' }] }],
    wordpress_runtime_prepare_steps: [prepareStep],
    wp_codebox_scenario_manifests: [{ id: 'consumer-owned-topology', url: '/' }],
  }, root);
  assert.equal(consumerTopology.inputs.extra_plugins.some((plugin) => plugin.slug === 'synthetic-network-fixture'), false);
  assert.equal(consumerTopology.workflow.steps.some((step) => step.args?.some((arg) => arg.includes('network-seed.php'))), false);
  assert.equal(consumerTopology.workflow.steps.some((step) => step.args?.some((arg) => arg.includes('network-assert.php'))), false);
  assert.equal(consumerTopology.workflow.steps.some((step) => step.command === 'wordpress.browser-probe'), false);
  assert.equal(consumerTopology.workflow.steps.some((step) => step.command === 'wordpress.browser-actions'), false);
  assert.equal(consumerTopology.workflow.steps.some((step) => step.command === 'wordpress.browser-scenario'), true);
  const consumerBench = consumerTopology.workflow.steps.find((step) => step.command === 'wordpress.bench');
  assert.ok(consumerBench.args.includes('plugin-slug=consumer-plugin'));
  assert.equal(consumerBench.args.some((arg) => arg.includes('synthetic-network-fixture')), false);

  const previousCodeboxBin = process.env.HOMEBOY_WP_CODEBOX_BIN;
  const previousMaxBuffer = process.env.HOMEBOY_WP_CODEBOX_MAX_BUFFER_BYTES;
  const cli = path.join(temporary, 'wp-codebox');
  await writeFile(cli, `#!/usr/bin/env node
const args = process.argv.slice(2);
if (args.includes('--version')) process.stdout.write('0.21.0');
else if (args.slice(-3).join(' ') === 'runtime descriptor --json') process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } }));
else if (args[0] === 'emit') process.stdout.write('x'.repeat(Number(args[1])));
else if (args[0] === 'overflow') { process.stderr.write('overflow diagnostic'); process.stdout.write('x'.repeat(Number(args[1]))); }
else process.exitCode = 1;
`);
  await chmod(cli, 0o755);
  process.env.HOMEBOY_WP_CODEBOX_BIN = cli;
  try {
    const largeOutput = runCodebox(['emit', String(2 * 1024 * 1024)], true);
    assert.equal(largeOutput.stdout.length, 2 * 1024 * 1024);
    process.env.HOMEBOY_WP_CODEBOX_MAX_BUFFER_BYTES = '1024';
    assert.throws(
      () => runCodebox(['overflow', '2048'], true),
      (error) => error.code === 'ENOBUFS'
        && error.maxBuffer === 1024
        && error.stderr.includes('overflow diagnostic')
        && error.stdout.length > 0,
    );
    process.env.HOMEBOY_WP_CODEBOX_MAX_BUFFER_BYTES = 'invalid';
    assert.throws(() => runCodebox(['--version'], true), /must be a positive integer/);
  } finally {
    if (previousCodeboxBin === undefined) {
      delete process.env.HOMEBOY_WP_CODEBOX_BIN;
    } else {
      process.env.HOMEBOY_WP_CODEBOX_BIN = previousCodeboxBin;
    }
    if (previousMaxBuffer === undefined) {
      delete process.env.HOMEBOY_WP_CODEBOX_MAX_BUFFER_BYTES;
    } else {
      process.env.HOMEBOY_WP_CODEBOX_MAX_BUFFER_BYTES = previousMaxBuffer;
    }
  }

  await assert.rejects(buildRecipe({ wordpress_runtime_php_version: '' }, root), /must be a non-empty/);
  await assert.rejects(buildRecipe({ wordpress_runtime_php_version: '8.4.1' }, root), /Unsupported/);
  await assert.rejects(buildRecipe({ wordpress_runtime_php_version: '8.6' }, root), /Unsupported/);
  await assert.rejects(buildRecipe({ wordpress_runtime_php_version: '5.2' }, root), /Unsupported/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: {} }, root), /must be an array/);
  await assert.rejects(buildRecipe({ wp_codebox_dependency_overlays: {} }, root), /wp_codebox_dependency_overlays must be an array/);
  await assert.rejects(buildRecipe({ wordpress_multisite_synthetic_fixture: false, wordpress_runtime_workloads: [{ id: 'missing-owner', run: [] }] }, root), /workload_plugin_slug is required/);
  await assert.rejects(buildRecipe({ wordpress_multisite_synthetic_fixture: false, wordpress_runtime_workload_plugin_slug: '../plugin', wordpress_runtime_workloads: [{ id: 'invalid-owner', run: [] }] }, root), /valid WordPress plugin directory slug/);
  await assert.rejects(buildRecipe({ wordpress_multisite_synthetic_fixture: false, wordpress_runtime_workload_plugin_slug: 'missing-plugin', wordpress_runtime_workloads: [{ id: 'unknown-owner', run: [] }] }, root), /must match a declared/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [null] }, root), /must be an object/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: 'relative/theme', slug: 'theme' }] }, root), /absolute path/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: path.join(temporary, 'missing'), slug: 'theme' }] }, root), /does not exist/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: notDirectory, slug: 'theme' }] }, root), /must be a directory/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: noStyleTheme, slug: 'theme' }] }, root), /must contain style\.css/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: noHeaderTheme, slug: 'theme' }] }, root), /non-empty Theme Name header/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: emptyHeaderTheme, slug: 'theme' }] }, root), /non-empty Theme Name header/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: lateHeaderTheme, slug: 'theme' }] }, root), /first 8 KB/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: noEntrypointTheme, slug: 'theme' }] }, root), /standalone theme must contain/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: theme, slug: '../theme' }] }, root), /valid WordPress theme directory slug/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: theme, slug: 'theme', metadata: [] }] }, root), /metadata must be an object/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: theme, slug: 'theme', activate: 'yes' }] }, root), /activate must be a boolean/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [
    { source: theme, slug: 'duplicate' },
    { source: secondTheme, slug: 'duplicate' },
  ] }, root), /duplicate slug/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [
    { source: theme, slug: 'consumer-theme', activate: true },
    { source: secondTheme, slug: 'second-theme', activate: true },
  ] }, root), /at most one active theme/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [
    { source: childTheme, slug: 'child-theme' },
  ] }, root), /requires mounted parent theme/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [
    { source: theme, slug: 'consumer-theme', activate: true },
  ], wordpress_runtime_blueprint: { steps: [
    { step: 'defineWpConfigConsts', consts: { WP_DEFAULT_THEME: 'consumer-theme' } },
    { step: 'defineWpConfigConsts', consts: { WP_DEFAULT_THEME: 'conflicting-theme' } },
  ] } }, root), /WP_DEFAULT_THEME must match/);

  const rig = JSON.parse(await readFile(path.join(root, 'rig.json'), 'utf8'));
  assert.equal(rig.id, 'wordpress-multisite-e2e');
  assert.ok(rig.pipeline.up.some((step) => step.command?.includes('run.mjs')));
  assert.ok(rig.pipeline.check.some((step) => step.command?.includes('--dry-run')));
} finally {
  await rm(temporary, { recursive: true, force: true });
}

console.log('WordPress multisite E2E rig contracts passed.');
