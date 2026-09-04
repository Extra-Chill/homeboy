/**
 * Regression: a component whose tests span several PHPUnit configs must run
 * every declared suite in one managed pass, report each individually, and keep
 * failing when any one of them fails.
 *
 * Before wp_codebox_phpunit_suites existed the runner accepted one config, so
 * such components invoked PHPUnit directly outside the managed runner and their
 * results carried no structured counts or per-suite attribution.
 *
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-multi-suite-'));
const component = path.join(root, 'sample-plugin');
const cli = path.join(root, 'wp-codebox');

await mkdir(path.join(component, 'tests/Unit'), { recursive: true });
await writeFile(path.join(component, 'sample-plugin.php'), '<?php\n/* Plugin Name: Sample Plugin */\n');
await writeFile(path.join(component, 'tests/Unit/FirstTest.php'), '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class FirstTest extends TestCase {}\n');
for (const name of ['phpunit-alpha.xml.dist', 'phpunit-beta.xml.dist']) {
  await writeFile(path.join(component, name), '<?xml version="1.0"?>\n<phpunit><testsuites><testsuite name="s"><directory>tests</directory></testsuite></testsuites></phpunit>\n');
}

// The fake CLI records every recipe build so the test can assert which config
// each suite ran with, and fails only the suite named by FAILING_SUITE.
await writeFile(cli, `#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
const args = process.argv.slice(2);
if (args[0] === '--version') { process.stdout.write('0.21.0\\n'); process.exit(0); }
if (args.slice(-3).join(' ') === 'runtime descriptor --json') {
  process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } }));
  process.exit(0);
}
if (args[0] === 'recipe' && args[1] === 'build') {
  const options = JSON.parse(fs.readFileSync(args[args.indexOf('--options') + 1], 'utf8'));
  fs.appendFileSync(process.env.CAPTURED_CONFIGS, options.phpunitXml + '\\n');
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
const artifactRoot = args[args.indexOf('--artifacts') + 1];
const runtime = path.join(artifactRoot, 'runtime-fixture');
fs.mkdirSync(path.join(runtime, 'files', 'phpunit'), { recursive: true });
fs.writeFileSync(path.join(artifactRoot, 'latest-runtime.json'), JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
// Suite order is recorded, so the artifact directory suffix identifies which
// suite this invocation belongs to.
const failing = process.env.FAILING_SUITE || '';
const failed = failing !== '' && artifactRoot.endsWith(failing);
fs.writeFileSync(path.join(runtime, 'files', 'phpunit', '.pg-test-result.txt'), [
  'PLUGIN_DETECTED sample-plugin.php',
  'PLUGIN_ACTIVATE sample-plugin/sample-plugin.php',
  'PLUGIN_ACTIVATE_OK sample-plugin/sample-plugin.php stage=activation',
  'DISCOVERY: dirs=/wordpress/wp-content/plugins/sample-plugin/tests files=1 suffixes=Test.php prefixes=test- excludes=0 found=1',
  '',
].join('\\n'));
fs.writeFileSync(path.join(runtime, 'files', 'test-results.json'), JSON.stringify({
  schema: 'wp-codebox/test-results/v1',
  status: failed ? 'failed' : 'passed',
  summary: { total: 2, passed: failed ? 0 : 2, failed: failed ? 2 : 0, skipped: 0, unknown: 0 },
  suites: [],
}));
process.stdout.write(JSON.stringify({
  success: !failed,
  executions: [{ command: 'wordpress.phpunit', exitCode: failed ? 2 : 0, stdout: failed ? 'Tests: 2, Assertions: 2, Failures: 2.\\n' : 'OK (2 tests, 2 assertions)\\n', stderr: '' }],
}));
process.exitCode = failed ? 2 : 0;
`);
await chmod(cli, 0o755);

function execute(name, settings, { failingSuite = '' } = {}) {
  const capturedConfigs = path.join(root, `${name}-configs.txt`);
  const run = spawnSync('bash', [runner], {
    env: {
      ...process.env,
      CAPTURED_CONFIGS: capturedConfigs,
      FAILING_SUITE: failingSuite,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'sample-plugin',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, `${name}-artifacts`),
      HOMEBOY_SETTINGS_JSON: JSON.stringify({ phpunit_no_tests: 'fail', wp_codebox_phpunit_bootstrap_mode: 'managed', ...settings }),
    },
    encoding: 'utf8',
  });
  return { run, capturedConfigs };
}

// --- Every declared suite runs and reports individually --------------------
{
  const { run, capturedConfigs } = execute('multi', {
    wp_codebox_phpunit_suites: [
      { name: 'alpha', config: 'phpunit-alpha.xml.dist' },
      { name: 'beta', config: 'phpunit-beta.xml.dist' },
    ],
  });
  assert.equal(run.status, 0, `expected a passing multi-suite run, got ${run.status}\n${run.stdout}\n${run.stderr}`);
  assert.match(run.stdout, /PHPUNIT_SUITE_RESULT:name=alpha status=passed/);
  assert.match(run.stdout, /PHPUNIT_SUITE_RESULT:name=beta status=passed/);
  const configs = (await readFile(capturedConfigs, 'utf8')).trim().split('\n');
  assert.equal(configs.length, 2, `expected one recipe per suite, got ${configs.length}`);
  assert.match(configs[0], /phpunit-alpha\.xml\.dist$/);
  assert.match(configs[1], /phpunit-beta\.xml\.dist$/);
}

// --- A failing suite names itself and fails the phase ----------------------
// The failure is in the *second* suite, which also proves the first suite's
// success cannot mask a later failure.
{
  const { run } = execute('multi-fail', {
    wp_codebox_phpunit_suites: [
      { name: 'alpha', config: 'phpunit-alpha.xml.dist' },
      { name: 'beta', config: 'phpunit-beta.xml.dist' },
    ],
  }, { failingSuite: 'beta' });
  assert.notEqual(run.status, 0, 'expected a failing suite to fail the phase');
  assert.match(run.stdout, /PHPUNIT_SUITE_RESULT:name=alpha status=passed/);
  assert.match(run.stdout, /PHPUNIT_SUITE_RESULT:name=beta status=failed/);
  assert.doesNotMatch(run.stdout, /WP Codebox test run complete/);
}

// --- An early failure must not skip later suites ---------------------------
{
  const { run, capturedConfigs } = execute('multi-fail-first', {
    wp_codebox_phpunit_suites: [
      { name: 'alpha', config: 'phpunit-alpha.xml.dist' },
      { name: 'beta', config: 'phpunit-beta.xml.dist' },
    ],
  }, { failingSuite: 'alpha' });
  assert.notEqual(run.status, 0, 'expected a failing first suite to fail the phase');
  assert.match(run.stdout, /PHPUNIT_SUITE_RESULT:name=alpha status=failed/);
  assert.match(run.stdout, /PHPUNIT_SUITE_RESULT:name=beta status=passed/);
  const configs = (await readFile(capturedConfigs, 'utf8')).trim().split('\n');
  assert.equal(configs.length, 2, 'a failing suite must not prevent later suites from running');
}

// --- Single-config components are unaffected -------------------------------
{
  const { run, capturedConfigs } = execute('single', { wp_codebox_phpunit_config: 'phpunit-alpha.xml.dist' });
  assert.equal(run.status, 0, `expected the legacy single-config path to keep passing, got ${run.status}\n${run.stderr}`);
  assert.doesNotMatch(run.stdout, /PHPUNIT_SUITE_RESULT/, 'an unnamed single suite must not emit per-suite lines');
  assert.match(run.stdout, /WP Codebox test run complete/);
  const configs = (await readFile(capturedConfigs, 'utf8')).trim().split('\n');
  assert.equal(configs.length, 1);
}

// --- Declaring both settings is a configuration error ----------------------
{
  const { run } = execute('conflict', {
    wp_codebox_phpunit_config: 'phpunit-alpha.xml.dist',
    wp_codebox_phpunit_suites: [{ name: 'alpha', config: 'phpunit-alpha.xml.dist' }],
  });
  assert.notEqual(run.status, 0, 'expected declaring both settings to fail');
  assert.match(`${run.stdout}${run.stderr}`, /not both/);
}

// --- Malformed suite declarations are rejected -----------------------------
for (const [label, suites, expected] of [
  ['missing name', [{ config: 'phpunit-alpha.xml.dist' }], /name must match/],
  ['missing config', [{ name: 'alpha' }], /config is required/],
  ['duplicate name', [{ name: 'a', config: 'phpunit-alpha.xml.dist' }, { name: 'a', config: 'phpunit-beta.xml.dist' }], /duplicate suite name/],
  ['not an array', 'phpunit-alpha.xml.dist', /must be an array/],
]) {
  const { run } = execute(`invalid-${label.replace(/\s+/g, '-')}`, { wp_codebox_phpunit_suites: suites });
  assert.notEqual(run.status, 0, `expected ${label} to fail`);
  assert.match(`${run.stdout}${run.stderr}`, expected, `unexpected diagnostic for ${label}`);
}

await rm(root, { recursive: true, force: true });
process.stdout.write('WP Codebox PHPUnit multi-suite smoke passed\n');
