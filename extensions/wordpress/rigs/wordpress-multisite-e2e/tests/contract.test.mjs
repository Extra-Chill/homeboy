/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
/**
 * Internal dependencies
 */
import { buildRecipe } from '../run.mjs';

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const temporary = await mkdtemp(path.join(os.tmpdir(), 'wordpress-multisite-e2e-contract-'));
const theme = path.join(temporary, 'consumer-theme');
const secondTheme = path.join(temporary, 'second-theme');
const noHeaderTheme = path.join(temporary, 'no-header-theme');
const noStyleTheme = path.join(temporary, 'no-style-theme');
const notDirectory = path.join(temporary, 'not-directory');

try {
  await Promise.all([mkdir(theme), mkdir(secondTheme), mkdir(noHeaderTheme), mkdir(noStyleTheme)]);
  await Promise.all([
    writeFile(path.join(theme, 'style.css'), '/*\nTheme Name: Consumer Theme\n*/\n'),
    writeFile(path.join(secondTheme, 'style.css'), '/*\n * Theme Name: Second Theme\n */\n'),
    writeFile(path.join(noHeaderTheme, 'style.css'), '/* No theme header. */\n'),
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
  assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.bench'));
  assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.browser-scenario'));
  assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.browser-probe'));
  assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.browser-actions'));
  const seedIndex = recipe.workflow.steps.findIndex((step) => step.args?.some((arg) => arg.includes('network-seed.php')));
  const activationIndex = recipe.workflow.steps.findIndex((step) => step.metadata?.kind === 'wordpress-theme-activation');
  const prepareIndex = recipe.workflow.steps.indexOf(prepareStep);
  assert.ok(seedIndex >= 0 && seedIndex < activationIndex && activationIndex < prepareIndex);
  assert.ok(recipe.workflow.steps[activationIndex].args[0].includes("switch_theme( $theme_slug )"));
  assert.ok(recipe.workflow.steps.some((step) => step.args?.some((arg) => arg.includes('network-assert.php'))));

  const withoutRuntimeInputs = await buildRecipe({}, root);
  assert.equal(Object.hasOwn(withoutRuntimeInputs.runtime, 'phpVersion'), false);
  assert.deepEqual(withoutRuntimeInputs.inputs.mounts, []);
  assert.equal(withoutRuntimeInputs.workflow.steps.some((step) => step.metadata?.kind === 'wordpress-theme-activation'), false);

  await assert.rejects(buildRecipe({ wordpress_runtime_php_version: '' }, root), /must be a non-empty/);
  await assert.rejects(buildRecipe({ wordpress_runtime_php_version: '8.4.1' }, root), /Unsupported/);
  await assert.rejects(buildRecipe({ wordpress_runtime_php_version: '8.6' }, root), /Unsupported/);
  await assert.rejects(buildRecipe({ wordpress_runtime_php_version: '5.2' }, root), /Unsupported/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: {} }, root), /must be an array/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [null] }, root), /must be an object/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: 'relative/theme', slug: 'theme' }] }, root), /absolute path/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: path.join(temporary, 'missing'), slug: 'theme' }] }, root), /does not exist/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: notDirectory, slug: 'theme' }] }, root), /must be a directory/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: noStyleTheme, slug: 'theme' }] }, root), /must contain style\.css/);
  await assert.rejects(buildRecipe({ wp_codebox_extra_themes: [{ source: noHeaderTheme, slug: 'theme' }] }, root), /Theme Name header/);
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

  const rig = JSON.parse(await readFile(path.join(root, 'rig.json'), 'utf8'));
  assert.equal(rig.id, 'wordpress-multisite-e2e');
  assert.ok(rig.pipeline.up.some((step) => step.command?.includes('run.mjs')));
  assert.ok(rig.pipeline.check.some((step) => step.command?.includes('--dry-run')));
} finally {
  await rm(temporary, { recursive: true, force: true });
}

console.log('WordPress multisite E2E rig contracts passed.');
