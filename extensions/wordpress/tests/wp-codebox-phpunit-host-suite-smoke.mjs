/**
 * Regression: a component whose suites mix sandboxed WordPress integration
 * tests with plain host-PHP unit tests must be able to say which is which.
 *
 * Every declared suite previously ran inside a WP Codebox sandbox. Suites whose
 * tests bootstrap without WordPress matched none of the sandbox's discovery and
 * reported PHPUNIT_ZERO_TESTS, so components kept them in a raw shell step
 * outside the managed runner where they produced no counts and no attribution.
 *
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-host-suite-'));
const component = path.join(root, 'sample-plugin');
const cli = path.join(root, 'wp-codebox');

await mkdir(path.join(component, 'tests/Unit'), { recursive: true });
await mkdir(path.join(component, 'vendor/bin'), { recursive: true });
await writeFile(path.join(component, 'sample-plugin.php'), '<?php\n/* Plugin Name: Sample Plugin */\n');
await writeFile(path.join(component, 'tests/Unit/FirstTest.php'), '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class FirstTest extends TestCase {}\n');

// A WordPress bootstrap and a standalone one, so runtime inference has real
// files to read rather than a stubbed answer.
await writeFile(path.join(component, 'tests/wp-bootstrap.php'), '<?php\nrequire getenv( "WP_TESTS_DIR" ) . "/includes/bootstrap.php";\n');
// The standalone bootstrap mentions WP_UnitTestCase in a docblock while
// deliberately not using it, which is what real components do. Inference must
// read code rather than comments or this suite is misclassified.
await writeFile(
  path.join(component, 'tests/plain-bootstrap.php'),
  '<?php\n/**\n * Existing tests extend WP_UnitTestCase and need WP_TESTS_DIR.\n * These do not.\n */\nrequire __DIR__ . "/../vendor/autoload.php";\n',
);
await writeFile(
  path.join(component, 'phpunit-sandbox.xml.dist'),
  '<?xml version="1.0"?>\n<phpunit bootstrap="tests/wp-bootstrap.php"><testsuites><testsuite name="s"><directory>tests</directory></testsuite></testsuites></phpunit>\n',
);
await writeFile(
  path.join(component, 'phpunit-unit.xml.dist'),
  '<?xml version="1.0"?>\n<phpunit bootstrap="tests/plain-bootstrap.php"><testsuites><testsuite name="u"><directory>tests</directory></testsuite></testsuites></phpunit>\n',
);

// The component's own PHPUnit. A host suite must invoke this rather than the
// sandbox, so it records that it ran and honours HOST_PHPUNIT_OUTPUT.
await writeFile(path.join(component, 'vendor/bin/phpunit'), `#!/usr/bin/env node
import fs from 'node:fs';
fs.appendFileSync(process.env.CAPTURED_HOST_RUNS, process.argv.slice(2).join(' ') + '\\n');
process.stdout.write(process.env.HOST_PHPUNIT_OUTPUT || 'OK (7 tests, 9 assertions)\\n');
process.exitCode = Number(process.env.HOST_PHPUNIT_STATUS || 0);
`);
await chmod(path.join(component, 'vendor/bin/phpunit'), 0o755);

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
fs.writeFileSync(path.join(runtime, 'files', 'phpunit', '.pg-test-result.txt'), [
  'PLUGIN_DETECTED sample-plugin.php',
  'PLUGIN_ACTIVATE sample-plugin/sample-plugin.php',
  'PLUGIN_ACTIVATE_OK sample-plugin/sample-plugin.php stage=activation',
  'DISCOVERY: dirs=/wordpress/wp-content/plugins/sample-plugin/tests files=1 suffixes=Test.php prefixes=test- excludes=0 found=1',
  '',
].join('\\n'));
fs.writeFileSync(path.join(runtime, 'files', 'test-results.json'), JSON.stringify({
  schema: 'wp-codebox/test-results/v1',
  status: 'passed',
  summary: { total: 2, passed: 2, failed: 0, skipped: 0, unknown: 0 },
  suites: [],
}));
process.stdout.write(JSON.stringify({
  success: true,
  executions: [{ command: 'wordpress.phpunit', exitCode: 0, stdout: 'OK (2 tests, 2 assertions)\\n', stderr: '' }],
}));
`);
await chmod(cli, 0o755);

function execute(name, settings, { hostOutput = '', hostStatus = '0' } = {}) {
  const capturedConfigs = path.join(root, `${name}-configs.txt`);
  const capturedHostRuns = path.join(root, `${name}-host-runs.txt`);
  const run = spawnSync('bash', [runner], {
    env: {
      ...process.env,
      CAPTURED_CONFIGS: capturedConfigs,
      CAPTURED_HOST_RUNS: capturedHostRuns,
      HOST_PHPUNIT_OUTPUT: hostOutput,
      HOST_PHPUNIT_STATUS: hostStatus,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'sample-plugin',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, `${name}-artifacts`),
      HOMEBOY_SETTINGS_JSON: JSON.stringify({ phpunit_no_tests: 'fail', wp_codebox_phpunit_bootstrap_mode: 'managed', ...settings }),
    },
    encoding: 'utf8',
  });
  return { run, capturedConfigs, capturedHostRuns };
}

// --- A host suite runs on the host and reports like any other suite --------
{
  const { run, capturedConfigs, capturedHostRuns } = execute('host', {
    wp_codebox_phpunit_suites: [
      { name: 'sandbox-suite', config: 'phpunit-sandbox.xml.dist' },
      { name: 'unit', config: 'phpunit-unit.xml.dist', runtime: 'host' },
    ],
  });
  assert.equal(run.status, 0, `expected a passing mixed-runtime run, got ${run.status}\n${run.stdout}\n${run.stderr}`);
  assert.match(run.stdout, /PHPUNIT_SUITE_RESULT:name=sandbox-suite status=passed/);
  assert.match(run.stdout, /PHPUNIT_SUITE_RESULT:name=unit status=passed/);

  // The host suite must not have been built as a sandbox recipe.
  const configs = (await readFile(capturedConfigs, 'utf8')).trim().split('\n');
  assert.equal(configs.length, 1, `only the sandbox suite may build a recipe, got ${configs.length}`);
  assert.match(configs[0], /phpunit-sandbox\.xml\.dist$/);

  // It must have invoked the component's own PHPUnit with its own config.
  const hostRuns = (await readFile(capturedHostRuns, 'utf8')).trim().split('\n');
  assert.equal(hostRuns.length, 1, `expected exactly one host invocation, got ${hostRuns.length}`);
  assert.match(hostRuns[0], /--configuration phpunit-unit\.xml\.dist/);
}

// --- A failing host suite fails the phase and names itself -----------------
{
  const { run } = execute('host-fail', {
    wp_codebox_phpunit_suites: [
      { name: 'sandbox-suite', config: 'phpunit-sandbox.xml.dist' },
      { name: 'unit', config: 'phpunit-unit.xml.dist', runtime: 'host' },
    ],
  }, { hostOutput: 'Tests: 7, Assertions: 9, Failures: 2.\n', hostStatus: '2' });
  assert.notEqual(run.status, 0, 'expected a failing host suite to fail the phase');
  assert.match(run.stdout, /PHPUNIT_SUITE_RESULT:name=sandbox-suite status=passed/);
  assert.match(run.stdout, /PHPUNIT_SUITE_RESULT:name=unit status=failed/);
}

// --- A host suite that executes nothing is a failure, not a pass -----------
// This is the exact condition that made the sandbox misclassification look
// green-adjacent: zero tests must never read as success.
{
  const { run } = execute('host-empty', {
    wp_codebox_phpunit_suites: [
      { name: 'sandbox-suite', config: 'phpunit-sandbox.xml.dist' },
      { name: 'unit', config: 'phpunit-unit.xml.dist', runtime: 'host' },
    ],
  }, { hostOutput: 'No tests executed!\n', hostStatus: '0' });
  assert.notEqual(run.status, 0, 'a host suite executing zero tests must fail');
  assert.match(run.stdout, /executed zero tests/);
}

// --- A runtime contradicting its config fails with a specific diagnostic ---
// Inference alone previously produced seven silently failing suites; declaring
// the wrong runtime must produce one clear error instead.
{
  const { run } = execute('host-mismatch', {
    wp_codebox_phpunit_suites: [
      { name: 'sandbox-suite', config: 'phpunit-sandbox.xml.dist' },
      { name: 'unit', config: 'phpunit-unit.xml.dist' },
    ],
  });
  assert.notEqual(run.status, 0, 'a standalone config declared as sandbox must fail');
  assert.match(`${run.stdout}${run.stderr}`, /bootstraps without WordPress; declare it as "host"/);
}

{
  const { run } = execute('sandbox-mismatch', {
    wp_codebox_phpunit_suites: [
      { name: 'sandbox-suite', config: 'phpunit-sandbox.xml.dist', runtime: 'host' },
      { name: 'unit', config: 'phpunit-unit.xml.dist', runtime: 'host' },
    ],
  });
  assert.notEqual(run.status, 0, 'a WordPress config declared as host must fail');
  assert.match(`${run.stdout}${run.stderr}`, /bootstraps WordPress; declare it as "sandbox"/);
}

// --- Discovery needs a sandbox suite ---------------------------------------
{
  const { run } = execute('host-only', {
    wp_codebox_phpunit_suites: [
      { name: 'unit', config: 'phpunit-unit.xml.dist', runtime: 'host' },
    ],
  });
  assert.notEqual(run.status, 0, 'a host-only declaration must fail rather than skip discovery');
  assert.match(`${run.stdout}${run.stderr}`, /at least one sandbox suite/);
}

process.stdout.write('wp-codebox-phpunit-host-suite-smoke: ok\n');
