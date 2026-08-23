/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

// Exercises the plugin PHPUnit topology handoff. WP Codebox owns Playground's
// multisite preinstall, network activation, and PHPUnit bootstrap lifecycle.

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-multisite-'));

// Minimal fake wp-codebox CLI: capture the phpunit options JSON, then satisfy
// the recipe-run handoff the adapter expects.
const cli = path.join(root, 'wp-codebox');
await writeFile(cli, `#!/usr/bin/env node
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises';
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args.slice(-3).join(' ') === 'runtime descriptor --json') { process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } })); process.exit(0); }
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
  await writeFile(artifacts + '/runtime-fixture/files/test-results.json', JSON.stringify({ schema: 'wp-codebox/test-results/v1', status: 'passed', summary: { total: 1, passed: 1, failed: 0, skipped: 0 }, suites: [], rawLogReferences: [] }));
  process.stdout.write(JSON.stringify({ success: true }));
}
`);
await chmod(cli, 0o755);

let scenario = 0;
async function resolvedOptions({ fixture, settings = {}, env = {} }) {
  scenario += 1;
  const component = path.join(extension, 'tests', 'fixtures', 'wp-codebox-phpunit-topology', fixture);
  const observed = path.join(root, `observed-${scenario}.jsonl`);
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
  // Default: no signal anywhere -> single-site.
  const singleSite = await resolvedOptions({ fixture: 'single-site' });
  assert.equal(
    singleSite.multisite,
    false,
    'no signal defaults to single-site',
  );

  // A non-network plugin header precedes the network entrypoint in this fixture.
  // Detection must inspect every supported plugin header before selecting topology.
  assert.equal(
    (await resolvedOptions({ fixture: 'network-plugin' })).multisite,
    true,
    'Network: true plugin header auto-enables multisite',
  );

  // wp_codebox_multisite setting enables multisite for a single-site plugin.
  assert.equal(
    (await resolvedOptions({ fixture: 'explicit-multisite', settings: { wp_codebox_multisite: true } })).multisite,
    true,
    'wp_codebox_multisite setting enables multisite',
  );

  // Setting can also explicitly force single-site even for a Network plugin.
  assert.equal(
    (await resolvedOptions({ fixture: 'network-plugin', settings: { wp_codebox_multisite: false } })).multisite,
    false,
    'explicit wp_codebox_multisite=false overrides Network header',
  );

  // Env var takes precedence over everything.
  assert.equal(
    (await resolvedOptions({
      fixture: 'single-site',
      settings: { wp_codebox_multisite: false },
      env: { HOMEBOY_WORDPRESS_MULTISITE: '1' },
    })).multisite,
    true,
    'HOMEBOY_WORDPRESS_MULTISITE env overrides settings',
  );

  const defaultDatabase = singleSite;
  assert.equal('databaseType' in defaultDatabase, false, 'omitted database_type preserves WP Codebox defaults');
  assert.equal(defaultDatabase.extra_plugins.length, 1, 'no dependencies only emits the primary plugin');
  assert.equal(defaultDatabase.extra_plugins[0].activate, true, 'the plugin under review is activated alongside its validation dependencies');

  const mysqlDatabase = await resolvedOptions({ fixture: 'single-site', settings: { database_type: 'mysql' } });
  assert.equal(mysqlDatabase.databaseType, 'mysql', 'database_type maps to the WP Codebox databaseType contract');

  const defaultPhp = singleSite;
  assert.equal('phpVersion' in defaultPhp, false, 'omitted runtime PHP version preserves WP Codebox defaults');

  const configuredPhp = await resolvedOptions({ fixture: 'single-site', settings: { wordpress_runtime_php_version: '8.4' } });
  assert.equal(configuredPhp.phpVersion, '8.4', 'configured runtime PHP version maps to the WP Codebox phpVersion contract');
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit multisite smoke passed.');
