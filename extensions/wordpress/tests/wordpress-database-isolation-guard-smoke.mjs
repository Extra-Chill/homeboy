/**
 * Regression smoke for Extra-Chill/homeboy#18886.
 *
 * The WordPress PHPUnit and bench runners must refuse to run when the database
 * target could come from anywhere other than the database they provision: a DB
 * constant defined ahead of the runner's wp-tests-config (wp_config_defines), a
 * DB_* value exported into the sandbox (bench_env), or a DB_* variable inherited
 * from the host environment. A normal isolated run must still pass.
 */

/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
/**
 * Internal dependencies
 */
import {
  DATABASE_TARGET_NAMES,
  TESTS_CONFIG_REDIRECT_NAMES,
  assertIsolatedDatabaseTarget,
  databaseIsolationViolations,
} from '../scripts/lib/wordpress-database-isolation-guard.mjs';

const extension = path.resolve(import.meta.dirname, '..');
const phpunitRunner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const benchAdapter = path.join(extension, 'scripts/bench/wp-codebox-bench-adapter.mjs');
const root = await mkdtemp(path.join(os.tmpdir(), 'wordpress-database-isolation-guard-'));
const component = path.join(root, 'plugin');
const cli = path.join(root, 'wp-codebox');
await mkdir(component, { recursive: true });
await writeFile(path.join(component, 'plugin.php'), '<?php\n/**\n * Plugin Name: Isolation Guard Test\n */\n');
await writeFile(cli, `#!/usr/bin/env node
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises';
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args[0] === 'runtime' && args[1] === 'descriptor') {
  process.stdout.write(JSON.stringify({
    schema: 'wp-codebox/runtime-descriptor/v1',
    readiness: { status: 'available', browserRuntime: { status: 'ready' } },
    capabilities: [],
    contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } },
  }));
  process.exit(0);
}
if (args[0] === 'recipe' && args[1] === 'build') {
  const options = JSON.parse(await readFile(args[args.indexOf('--options') + 1], 'utf8'));
  await appendFile(process.env.OBSERVED, JSON.stringify({ phase: 'build', options }) + '\\n');
  await writeFile(args[args.indexOf('--output') + 1], JSON.stringify({ schema: 'wp-codebox/workspace-recipe/v1', inputs: {} }));
  process.exit(0);
}
if (args[0] === 'recipe-run') {
  const artifacts = args[args.indexOf('--artifacts') + 1];
  await appendFile(process.env.OBSERVED, JSON.stringify({ phase: 'run' }) + '\\n');
  await mkdir(artifacts + '/runtime-fixture/files', { recursive: true });
  await writeFile(artifacts + '/latest-runtime.json', JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
  await writeFile(artifacts + '/runtime-fixture/files/test-results.json', JSON.stringify({ schema: 'wp-codebox/test-results/v1', status: 'passed', summary: { total: 1, passed: 1, failed: 0, skipped: 0 }, suites: [], rawLogReferences: [] }));
}
`);
await chmod(cli, 0o755);

// The smoke's own environment must not trip the guard it is testing.
const cleanEnvironment = Object.fromEntries(Object.entries(process.env)
  .filter(([name]) => !DATABASE_TARGET_NAMES.includes(name) && !TESTS_CONFIG_REDIRECT_NAMES.includes(name)));

let scenario = 0;
function invokePhpunit(settings, env = {}) {
  scenario += 1;
  const observed = path.join(root, `observed-${scenario}.jsonl`);
  const result = spawnSync(phpunitRunner, [], {
    env: {
      ...cleanEnvironment,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'isolation-guard-test',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_SETTINGS_JSON: JSON.stringify(settings),
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, `artifacts-${scenario}`),
      OBSERVED: observed,
      ...env,
    },
    encoding: 'utf8',
  });
  return { result, observed };
}

async function observations(file) {
  try {
    return (await readFile(file, 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
  } catch {
    return [];
  }
}

const liveDatabaseValue = 'live-site-database-must-not-leak';

async function expectRefused(invocation, sourcePattern, label) {
  assert.notEqual(invocation.result.status, 0, `${label}: runner must fail closed`);
  assert.match(invocation.result.stderr, /WordPress PHPUnit runner refused to run: the WordPress database target is not the isolated database/, `${label}: refusal is explicit`);
  assert.match(invocation.result.stderr, sourcePattern, `${label}: error names the offending source`);
  assert.deepEqual(await observations(invocation.observed), [], `${label}: refused before any recipe is built or run`);
  assert.equal(invocation.result.stdout.includes(liveDatabaseValue), false, `${label}: stdout omits the offending value`);
  assert.equal(invocation.result.stderr.includes(liveDatabaseValue), false, `${label}: stderr omits the offending value`);
}

try {
  // Normal isolated run: no foreign DB target anywhere, so the run proceeds and passes.
  const isolated = invokePhpunit({ database_type: 'sqlite', wp_config_defines: { MY_PLUGIN_MODE: 'test' }, bench_env: { MY_PLUGIN_FLAG: '1' } });
  assert.equal(isolated.result.status, 0, isolated.result.stderr);
  const isolatedObservations = await observations(isolated.observed);
  assert.deepEqual(isolatedObservations.map((entry) => entry.phase), ['build', 'run'], 'isolated run builds and executes the recipe');
  assert.deepEqual(isolatedObservations[0].options.wpConfigDefines, { MY_PLUGIN_MODE: 'test' }, 'unrelated wp_config_defines pass through');

  // A DB constant defined before the runner's own wp-tests-config DB constants.
  await expectRefused(
    invokePhpunit({ wp_config_defines: { DB_NAME: liveDatabaseValue } }),
    /wp_config_defines\.DB_NAME \(defined before the runner's wp-tests-config DB constants\)/,
    'pre-defined DB_NAME',
  );

  // DB_* inherited from the host environment running Homeboy.
  await expectRefused(
    invokePhpunit({}, { DB_NAME: liveDatabaseValue, DB_HOST: 'localhost' }),
    /inherited host environment variable DB_HOST; inherited host environment variable DB_NAME/,
    'inherited DB_* environment',
  );

  // DB_* exported into the sandbox, where the MySQL config path reads it.
  await expectRefused(
    invokePhpunit({ database_type: 'mysql', bench_env: { DB_HOST: liveDatabaseValue } }),
    /bench_env\.DB_HOST/,
    'bench_env DB_HOST',
  );

  // A host variable redirecting wp-phpunit to some other tests config.
  await expectRefused(
    invokePhpunit({}, { WP_PHPUNIT__TESTS_CONFIG: '/srv/www/live/wp-tests-config.php' }),
    /inherited host environment variable WP_PHPUNIT__TESTS_CONFIG/,
    'inherited tests-config redirect',
  );

  // Bench installs a site too, and fails closed on the same sources.
  const bench = spawnSync(process.execPath, [benchAdapter], {
    env: { ...cleanEnvironment, HOMEBOY_COMPONENT_PATH: component, HOMEBOY_SETTINGS_JSON: '{}', DB_USER: liveDatabaseValue },
    encoding: 'utf8',
  });
  assert.notEqual(bench.status, 0, 'bench runner must fail closed');
  assert.match(bench.stderr, /WordPress bench runner refused to run: .*inherited host environment variable DB_USER/);
  assert.equal(bench.stderr.includes(liveDatabaseValue), false, 'bench error omits the offending value');

  // Unit-level contract.
  assert.deepEqual(databaseIsolationViolations({ environment: { PATH: '/usr/bin', DB_SERVICE_HOST: 'x' }, settings: {} }), [], 'unrelated DB_SERVICE_* names are not database targets');
  assert.deepEqual(
    databaseIsolationViolations({ environment: { DB_HOST: 'admin.example' }, settings: {}, allowedEnvironmentNames: ['DB_HOST'] }),
    [],
    'declared database-service administration references are exempt',
  );
  assert.deepEqual(
    databaseIsolationViolations({ environment: {}, settings: { wp_config_defines: { db_password: 'x' } } }),
    ["wp_config_defines.db_password (defined before the runner's wp-tests-config DB constants)"],
    'define names are matched case-insensitively',
  );
  assert.throws(() => assertIsolatedDatabaseTarget({ environment: { DB_NAME: 'x' } }), /no fallback to a host database/);
  assert.doesNotThrow(() => assertIsolatedDatabaseTarget({ environment: {}, settings: {} }));
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WordPress database isolation guard smoke passed.');
