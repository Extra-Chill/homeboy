import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

// Exercises the plugin PHPUnit adapter's multisite resolution so that
// network-only plugins (Network: true) boot a multisite runtime instead of
// crashing in wp_die(). Covers the three supported sources, in precedence
// order: HOMEBOY_WORDPRESS_MULTISITE env, wp_codebox_multisite setting, and
// the plugin's own `Network: true` header.

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-multisite-'));

// Minimal fake wp-codebox CLI: capture the phpunit options JSON, then satisfy
// the recipe-run handoff the adapter expects.
const cli = path.join(root, 'wp-codebox');
await writeFile(cli, `#!/usr/bin/env node
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises';
const args = process.argv.slice(2);
if (args[0] === 'recipe' && args[1] === 'build') {
  const options = JSON.parse(await readFile(args[args.indexOf('--options') + 1], 'utf8'));
  await appendFile(process.env.OBSERVED, JSON.stringify({ options }) + '\\n');
  await writeFile(args[args.indexOf('--output') + 1], JSON.stringify({ schema: 'wp-codebox/workspace-recipe/v1' }));
  process.exit(0);
}
if (args[0] === 'recipe-run') {
  const artifacts = args[args.indexOf('--artifacts') + 1];
  await mkdir(artifacts + '/runtime-fixture/files', { recursive: true });
  await writeFile(artifacts + '/latest-runtime.json', JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
  process.stdout.write(JSON.stringify({ success: true }));
}
`);
await chmod(cli, 0o755);

let scenario = 0;
async function resolvedOptions({ pluginPhp, settings = {}, env = {} }) {
  scenario += 1;
  const component = path.join(root, `plugin-${scenario}`);
  const observed = path.join(root, `observed-${scenario}.jsonl`);
  await mkdir(component, { recursive: true });
  if (pluginPhp !== undefined) {
    await writeFile(path.join(component, 'plugin.php'), pluginPhp);
  }
  const result = spawnSync(runner, [], {
    env: {
      ...process.env,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: `plugin-${scenario}`,
      HOMEBOY_WP_CODEBOX_BIN: cli,
      OBSERVED: observed,
      HOMEBOY_SETTINGS_JSON: JSON.stringify(settings),
      ...env,
    },
    encoding: 'utf8',
  });
  assert.equal(result.status, 0, result.stderr);
  const captured = (await readFile(observed, 'utf8')).trim().split('\n').map(JSON.parse);
  return captured[0].options;
}

try {
  const singleSitePlugin = '<?php\n/**\n * Plugin Name: Single Site\n */\n';
  const networkPlugin = '<?php\n/**\n * Plugin Name: Network Only\n * Network: true\n */\n';

  // Default: no signal anywhere -> single-site.
  assert.equal(
    (await resolvedOptions({ pluginPhp: singleSitePlugin })).multisite,
    false,
    'no signal defaults to single-site',
  );

  // Plugin header Network: true auto-enables multisite with zero config.
  assert.equal(
    (await resolvedOptions({ pluginPhp: networkPlugin })).multisite,
    true,
    'Network: true plugin header auto-enables multisite',
  );

  // wp_codebox_multisite setting enables multisite for a single-site plugin.
  assert.equal(
    (await resolvedOptions({ pluginPhp: singleSitePlugin, settings: { wp_codebox_multisite: true } })).multisite,
    true,
    'wp_codebox_multisite setting enables multisite',
  );

  // Setting can also explicitly force single-site even for a Network plugin.
  assert.equal(
    (await resolvedOptions({ pluginPhp: networkPlugin, settings: { wp_codebox_multisite: false } })).multisite,
    false,
    'explicit wp_codebox_multisite=false overrides Network header',
  );

  // Env var takes precedence over everything.
  assert.equal(
    (await resolvedOptions({
      pluginPhp: singleSitePlugin,
      settings: { wp_codebox_multisite: false },
      env: { HOMEBOY_WORDPRESS_MULTISITE: '1' },
    })).multisite,
    true,
    'HOMEBOY_WORDPRESS_MULTISITE env overrides settings',
  );

  const defaultDatabase = await resolvedOptions({ pluginPhp: singleSitePlugin });
  assert.equal('databaseType' in defaultDatabase, false, 'omitted database_type preserves WP Codebox defaults');
  assert.equal(defaultDatabase.extra_plugins.length, 1, 'no dependencies only emits the primary plugin');
  assert.equal(defaultDatabase.extra_plugins[0].activate, false, 'primary plugin remains inactive for the managed PHPUnit bootstrap');

  const mysqlDatabase = await resolvedOptions({ pluginPhp: singleSitePlugin, settings: { database_type: 'mysql' } });
  assert.equal(mysqlDatabase.databaseType, 'mysql', 'database_type maps to the WP Codebox databaseType contract');

  const defaultPhp = await resolvedOptions({ pluginPhp: singleSitePlugin });
  assert.equal('phpVersion' in defaultPhp, false, 'omitted runtime PHP version preserves WP Codebox defaults');

  const configuredPhp = await resolvedOptions({ pluginPhp: singleSitePlugin, settings: { wordpress_runtime_php_version: '8.4' } });
  assert.equal(configuredPhp.phpVersion, '8.4', 'configured runtime PHP version maps to the WP Codebox phpVersion contract');
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit multisite smoke passed.');
