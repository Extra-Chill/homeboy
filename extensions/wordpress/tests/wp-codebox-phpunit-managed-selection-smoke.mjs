/**
 * Regression: a managed changed-test scope must reach WP Codebox as explicit
 * sandbox paths, retain nonzero counts, and remain fail-closed.
 *
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-managed-selection-'));
const component = path.join(root, 'sample-plugin');
const cli = path.join(root, 'wp-codebox');

await mkdir(path.join(component, 'tests/Unit/Deep'), { recursive: true });
await writeFile(path.join(component, 'sample-plugin.php'), '<?php\n/* Plugin Name: Sample Plugin */\n');
await writeFile(path.join(component, 'tests/Unit/FirstTest.php'), '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class FirstTest extends TestCase {}\n');
await writeFile(path.join(component, 'tests/Unit/Deep/SecondTest.php'), '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class SecondTest extends TestCase {}\n');
await writeFile(path.join(component, 'tests/standalone-smoke.php'), '<?php\necho "standalone smoke";\n');

await writeFile(cli, `#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
const args = process.argv.slice(2);
if (args[0] === '--version') {
  process.stdout.write('0.21.0\\n');
  process.exit(0);
}
if (args.slice(-3).join(' ') === 'runtime descriptor --json') {
  process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } }));
  process.exit(0);
}
if (args[0] === 'recipe' && args[1] === 'build') {
  fs.copyFileSync(args[args.indexOf('--options') + 1], process.env.CAPTURED_OPTIONS);
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
const artifactRoot = args[args.indexOf('--artifacts') + 1];
const runtime = path.join(artifactRoot, 'runtime-fixture');
fs.mkdirSync(path.join(runtime, 'files', 'phpunit'), { recursive: true });
fs.writeFileSync(path.join(artifactRoot, 'latest-runtime.json'), JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
const scenario = process.env.SELECTION_SCENARIO || 'passed';
const failed = scenario === 'command-failed';
const total = scenario === 'passed' ? 2 : 0;
fs.writeFileSync(path.join(runtime, 'files', 'phpunit', '.pg-test-result.txt'), [
  'PLUGIN_DETECTED sample-plugin.php',
  'PLUGIN_ACTIVATE sample-plugin/sample-plugin.php',
  'PLUGIN_ACTIVATE_OK sample-plugin/sample-plugin.php stage=activation',
  'SCOPED_TEST_FILES requested=2 matched=2',
  'DISCOVERY: dirs=/wordpress/wp-content/plugins/sample-plugin/tests files=2 suffixes=Test.php prefixes=test- excludes=0 found=2',
  '',
].join('\\n'));
fs.writeFileSync(path.join(runtime, 'files', 'test-results.json'), JSON.stringify({
  schema: 'wp-codebox/test-results/v1',
  status: failed ? 'failed' : (total > 0 ? 'passed' : 'skipped'),
  summary: { total, passed: total, failed: 0, skipped: 0, unknown: 0 },
  suites: [],
}));
const stdout = total > 0 ? 'OK (2 tests, 2 assertions)\\n' : 'Tests: 0, Assertions: 0.\\n';
process.stdout.write(JSON.stringify({
  success: !failed,
  executions: [{ command: 'wordpress.phpunit', exitCode: failed ? 2 : 0, stdout, stderr: failed ? 'runner failed\\n' : '' }],
}));
process.exitCode = failed ? 2 : 0;
`);
await chmod(cli, 0o755);

function execute(name, changed, scenario = 'passed', env = {}) {
  const artifacts = path.join(root, `${name}-artifacts`);
  const capturedOptions = path.join(root, `${name}-options.json`);
  const run = spawnSync('bash', [runner], {
    env: {
      ...process.env,
      CAPTURED_OPTIONS: capturedOptions,
      SELECTION_SCENARIO: scenario,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'sample-plugin',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts,
       HOMEBOY_SETTINGS_JSON: JSON.stringify({ phpunit_no_tests: 'fail', wp_codebox_phpunit_bootstrap_mode: 'managed' }),
       HOMEBOY_WORDPRESS_PHPUNIT_CHANGED_TEST_FILES: changed,
       ...env,
    },
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
  });
  return { artifacts, capturedOptions, run };
}

async function artifactResults(artifacts) {
  const runDirectory = (await readdir(artifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.'));
  assert.ok(runDirectory, 'expected a WP Codebox run artifact directory');
  return JSON.parse(await readFile(path.join(artifacts, runDirectory, 'runtime-fixture/files/test-results.json'), 'utf8'));
}

try {
  const selected = 'tests/Unit/FirstTest.php\ntests/Unit/Deep/SecondTest.php';
  const passed = execute('passed', selected);
  assert.equal(passed.run.status, 0, passed.run.stderr || passed.run.stdout);
  const options = JSON.parse(await readFile(passed.capturedOptions, 'utf8'));
  assert.deepEqual(options.changedTestFiles, [
    '/wordpress/wp-content/plugins/sample-plugin/tests/Unit/FirstTest.php',
    '/wordpress/wp-content/plugins/sample-plugin/tests/Unit/Deep/SecondTest.php',
  ]);
  assert.equal(options.bootstrapMode, 'managed');
  assert.deepEqual((await artifactResults(passed.artifacts)).summary, {
    total: 2,
    passed: 2,
    failed: 0,
    skipped: 0,
    unknown: 0,
  });

  const invalid = execute('standalone-script', `${selected}\ntests/standalone-smoke.php`);
  assert.notEqual(invalid.run.status, 0, 'a standalone PHP script must not enter the PHPUnit scope');
  assert.match(`${invalid.run.stdout}${invalid.run.stderr}`, /contains non-PHPUnit paths: tests\/standalone-smoke\.php/);

  const zero = execute('zero', selected, 'zero');
  assert.notEqual(zero.run.status, 0, 'a genuine zero-test result must fail closed');
  assert.equal((await artifactResults(zero.artifacts)).status, 'failed');
  assert.match(zero.run.stdout, /PHPUNIT_ZERO_TESTS/);

   const commandFailed = execute('command-failed', selected, 'command-failed');
   assert.notEqual(commandFailed.run.status, 0, 'a PHPUnit command failure must remain a failure');
   assert.equal((await artifactResults(commandFailed.artifacts)).status, 'failed');

   const managedSource = path.join(root, 'managed-runtime', 'source');
   const managedCli = path.join(managedSource, 'packages', 'cli', 'dist', 'index.js');
   await mkdir(path.dirname(managedCli), { recursive: true });
   await writeFile(managedCli, await readFile(cli));
   spawnSync('git', ['init', '-q'], { cwd: managedSource });
   spawnSync('git', ['add', '.'], { cwd: managedSource });
   spawnSync('git', ['-c', 'user.name=Test', '-c', 'user.email=test@example.test', 'commit', '-qm', 'fixture'], { cwd: managedSource });
   const sourceSha = spawnSync('git', ['rev-parse', 'HEAD'], { cwd: managedSource, encoding: 'utf8' }).stdout.trim();
   await writeFile(path.join(managedSource, '.homeboy-runtime-identity.json'), JSON.stringify({ schema: 'homeboy/wp-codebox-managed-runtime-identity/v1', source_sha: sourceSha, cli_sha256: createHash('sha256').update(await readFile(managedCli)).digest('hex'), required_capabilities: ['wp-codebox/browser-contained-site-open/v1'] }));
   await writeFile(managedCli, `${await readFile(managedCli, 'utf8')}\n// tampered after setup\n`);
   const tampered = execute('tampered-managed', selected, 'passed', {
     HOMEBOY_WP_CODEBOX_BIN: managedCli,
     HOMEBOY_WP_CODEBOX_INSTALL_DIR: path.dirname(managedSource),
   });
   assert.notEqual(tampered.run.status, 0, 'the adapter must reject a tampered managed resolved command');
   assert.match(`${tampered.run.stdout}${tampered.run.stderr}`, /wp_codebox_managed_source_identity_invalid/);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit managed selection smoke passed.');
