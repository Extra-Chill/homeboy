/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
/**
 * Internal dependencies
 */
import { buildRecipe } from '../run.mjs';

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const recipe = await buildRecipe({
  wordpress_runtime_blueprint: { steps: [{ step: 'setSiteOptions', options: { blogname: 'Fixture Network' } }] },
  wp_codebox_extra_plugins: [{ source: '/tmp/consumer-plugin', slug: 'consumer-plugin', activate: false }],
  wordpress_runtime_prepare_steps: [{ command: 'wordpress.wp-cli', args: ['command=site list'] }],
  wordpress_runtime_workloads: [{ id: 'consumer-workload', run: [] }],
  wp_codebox_scenario_manifests: [{ id: 'consumer-browser-journey', url: '/beta/' }],
  wordpress_runtime_post_steps: [{ command: 'wordpress.wp-cli', args: ['command=option get home'] }],
}, root);

assert.equal(recipe.schema, 'wp-codebox/workspace-recipe/v1');
assert.equal(recipe.runtime.preview.siteUrl, 'http://localhost');
assert.equal(recipe.runtime.blueprint.steps[0].step, 'enableMultisite');
assert.equal(recipe.inputs.extra_plugins[0].pluginFile, 'synthetic-network-fixture/network-fixture.php');
assert.equal(recipe.inputs.extra_plugins[0].activate, false);
assert.equal(recipe.inputs.extra_plugins[1].slug, 'consumer-plugin');
assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.bench'));
assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.browser-scenario'));
assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.browser-probe'));
assert.ok(recipe.workflow.steps.some((step) => step.command === 'wordpress.browser-actions'));
assert.ok(recipe.workflow.steps.some((step) => step.args?.some((arg) => arg.includes('network-seed.php'))));
assert.ok(recipe.workflow.steps.some((step) => step.args?.some((arg) => arg.includes('network-assert.php'))));

const rig = JSON.parse(await readFile(path.join(root, 'rig.json'), 'utf8'));
assert.equal(rig.id, 'wordpress-multisite-e2e');
assert.ok(rig.pipeline.up.some((step) => step.command?.includes('run.mjs')));
assert.ok(rig.pipeline.check.some((step) => step.command?.includes('--dry-run')));

console.log('WordPress multisite E2E rig contracts passed.');
