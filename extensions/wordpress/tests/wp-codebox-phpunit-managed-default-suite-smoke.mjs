/**
 * Regression: a plugin that declares managed PHPUnit bootstrap and no PHPUnit
 * config at all must run its own tests/ suite with a real count, not reach WP
 * Codebox with an empty config and report a shape-of-success zero.
 *
 * `resolveSuiteConfigHostPath` returned null for such a plugin, `phpunitXml`
 * reached WP Codebox empty, discovery found no members, and the recipe
 * dispatched no steps -- a `PHPUNIT_ZERO_TESTS cause=recipe_run_no_executions`
 * that read as a completed, passing run. `extrachill-analytics` hit exactly
 * this with 46 real test files and no phpunit.xml(.dist).
 *
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { access, chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-managed-default-'));
const cli = path.join(root, 'wp-codebox');

// A component shaped like extrachill-analytics: managed bootstrap, several
// real test files nested under tests/, and no phpunit.xml or phpunit.xml.dist
// anywhere in the source tree.
const withTests = path.join(root, 'with-tests');
await mkdir(path.join(withTests, 'tests/Unit/Deep'), { recursive: true });
await writeFile(path.join(withTests, 'with-tests.php'), '<?php\n/* Plugin Name: With Tests */\n');
await writeFile(path.join(withTests, 'tests/Unit/FirstTest.php'), '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class FirstTest extends TestCase {}\n');
await writeFile(path.join(withTests, 'tests/Unit/SecondTest.php'), '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class SecondTest extends TestCase {}\n');
await writeFile(path.join(withTests, 'tests/Unit/Deep/ThirdTest.php'), '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class ThirdTest extends TestCase {}\n');
// A non-test file under tests/ must not be treated as a suite member.
await writeFile(path.join(withTests, 'tests/bootstrap-helper.php'), '<?php\n// not a test\n');

// Same managed declaration, but no tests/ directory and no config: nothing to
// synthesize and nothing to run.
const withoutTests = path.join(root, 'without-tests');
await mkdir(withoutTests, { recursive: true });
await writeFile(path.join(withoutTests, 'without-tests.php'), '<?php\n/* Plugin Name: Without Tests */\n');

// Managed declaration with a tests/ directory that exists but has no matching
// *Test.php files: also nothing to synthesize.
const emptyTests = path.join(root, 'empty-tests');
await mkdir(path.join(emptyTests, 'tests'), { recursive: true });
await writeFile(path.join(emptyTests, 'empty-tests.php'), '<?php\n/* Plugin Name: Empty Tests */\n');
await writeFile(path.join(emptyTests, 'tests/fixture-data.json'), '{}');

// A component that declares wp_codebox_phpunit_config explicitly, with no
// phpunit.xml.dist on disk, so it must be completely unaffected: the declared
// value must reach WP Codebox unchanged and no synthesis mount is added.
const explicitConfig = path.join(root, 'explicit-config');
await mkdir(path.join(explicitConfig, 'tests'), { recursive: true });
await writeFile(path.join(explicitConfig, 'explicit-config.php'), '<?php\n/* Plugin Name: Explicit Config */\n');
await writeFile(path.join(explicitConfig, 'tests/OnlyTest.php'), '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class OnlyTest extends TestCase {}\n');
await writeFile(
  path.join(explicitConfig, 'phpunit-custom.xml.dist'),
  '<?xml version="1.0"?>\n<phpunit><testsuites><testsuite name="s"><directory>tests</directory></testsuite></testsuites></phpunit>\n',
);

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
  fs.writeFileSync(process.env.CAPTURED_OPTIONS, JSON.stringify(options));
  // The adapter's own temp directory -- and any synthesized config mounted
  // out of it -- is removed once the adapter process exits, before the test
  // can read it back. Copy it out now, while the mount source still exists.
  const synthesizedMount = (options.mounts || []).find((entry) => entry.target === options.phpunitXml);
  if (synthesizedMount && process.env.CAPTURED_SYNTHESIZED_CONFIG) {
    fs.writeFileSync(process.env.CAPTURED_SYNTHESIZED_CONFIG, fs.readFileSync(synthesizedMount.source, 'utf8'));
  }
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
const artifactRoot = args[args.indexOf('--artifacts') + 1];
const runtime = path.join(artifactRoot, 'runtime-fixture');
fs.mkdirSync(path.join(runtime, 'files', 'phpunit'), { recursive: true });
fs.writeFileSync(path.join(artifactRoot, 'latest-runtime.json'), JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
fs.writeFileSync(path.join(runtime, 'files', 'phpunit', '.pg-test-result.txt'), [
  'PLUGIN_DETECTED with-tests.php',
  'PLUGIN_ACTIVATE with-tests/with-tests.php',
  'PLUGIN_ACTIVATE_OK with-tests/with-tests.php stage=activation',
  'DISCOVERY: dirs=/wordpress/wp-content/plugins/with-tests/tests files=3 suffixes=Test.php prefixes=test- excludes=0 found=3',
  '',
].join('\\n'));
fs.writeFileSync(path.join(runtime, 'files', 'test-results.json'), JSON.stringify({
  schema: 'wp-codebox/test-results/v1',
  status: 'passed',
  summary: { total: 3, passed: 3, failed: 0, skipped: 0, unknown: 0 },
  suites: [],
}));
process.stdout.write(JSON.stringify({
  success: true,
  executions: [{ command: 'wordpress.phpunit', exitCode: 0, stdout: 'OK (3 tests, 3 assertions)\\n', stderr: '' }],
}));
`);
await chmod(cli, 0o755);

function execute(name, componentPath, settings = {}) {
  const artifacts = path.join(root, `${name}-artifacts`);
  const capturedOptions = path.join(root, `${name}-options.json`);
  const capturedSynthesizedConfig = path.join(root, `${name}-synthesized-config.xml`);
  const run = spawnSync('bash', [runner], {
    env: {
      ...process.env,
      CAPTURED_OPTIONS: capturedOptions,
      CAPTURED_SYNTHESIZED_CONFIG: capturedSynthesizedConfig,
      HOMEBOY_COMPONENT_PATH: componentPath,
      COMPONENT_ID: path.basename(componentPath),
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts,
      HOMEBOY_SETTINGS_JSON: JSON.stringify({ phpunit_no_tests: 'fail', wp_codebox_phpunit_bootstrap_mode: 'managed', ...settings }),
    },
    encoding: 'utf8',
  });
  return { artifacts, capturedOptions, capturedSynthesizedConfig, run };
}

async function artifactResults(artifacts) {
  const runDirectory = (await readdir(artifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.'));
  assert.ok(runDirectory, 'expected a WP Codebox run artifact directory');
  return JSON.parse(await readFile(path.join(artifacts, runDirectory, 'runtime-fixture/files/test-results.json'), 'utf8'));
}

async function exists(target) {
  try {
    await access(target);
    return true;
  } catch {
    return false;
  }
}

try {
  // --- managed bootstrap + tests present + no config -> real count ---------
  {
    const { run, capturedOptions, capturedSynthesizedConfig, artifacts } = execute('with-tests', withTests);
    assert.equal(run.status, 0, `expected a passing synthesized run, got ${run.status}\n${run.stdout}\n${run.stderr}`);
    const options = JSON.parse(await readFile(capturedOptions, 'utf8'));
    assert.match(options.phpunitXml, /\/with-tests\/wp-codebox-managed-phpunit-default\.xml$/, 'the synthesized config must be a sandbox path under the plugin mount');
    const mount = (options.mounts || []).find((entry) => entry.target === options.phpunitXml);
    assert.ok(mount, 'the synthesized config must be mounted into the sandbox at the path phpunitXml names');
    assert.equal(mount.mode, 'readonly');
    const generated = await readFile(capturedSynthesizedConfig, 'utf8');
    assert.match(generated, /<directory suffix="Test\.php">tests<\/directory>/);
    assert.deepEqual((await artifactResults(artifacts)).summary, { total: 3, passed: 3, failed: 0, skipped: 0, unknown: 0 });
  }

  // --- managed bootstrap + no discoverable tests -> early, clear error -----
  {
    const { run, capturedOptions } = execute('without-tests', withoutTests);
    assert.notEqual(run.status, 0, 'a managed component with nothing to run must fail');
    assert.doesNotMatch(run.stdout, /PHPUNIT_ZERO_TESTS/, 'must fail before a sandbox run ever reports Total: 0');
    assert.match(`${run.stdout}${run.stderr}`, /no PHPUnit config .* and no tests\/\*\*\/\*Test\.php files were found/);
    assert.equal(await exists(capturedOptions), false, 'discovery must fail before any recipe is ever built');
  }

  // --- managed bootstrap + tests/ with no matching files -> same failure ---
  {
    const { run, capturedOptions } = execute('empty-tests', emptyTests);
    assert.notEqual(run.status, 0, 'a managed component whose tests/ has no *Test.php files must fail');
    assert.doesNotMatch(run.stdout, /PHPUNIT_ZERO_TESTS/);
    assert.match(`${run.stdout}${run.stderr}`, /no PHPUnit config .* and no tests\/\*\*\/\*Test\.php files were found/);
    assert.equal(await exists(capturedOptions), false);
  }

  // --- explicit wp_codebox_phpunit_config is completely unaffected ---------
  {
    const { run, capturedOptions } = execute('explicit-config', explicitConfig, { wp_codebox_phpunit_config: 'phpunit-custom.xml.dist' });
    assert.equal(run.status, 0, `expected the declared config to keep passing, got ${run.status}\n${run.stdout}\n${run.stderr}`);
    const options = JSON.parse(await readFile(capturedOptions, 'utf8'));
    assert.equal(options.phpunitXml, 'phpunit-custom.xml.dist', 'a declared config must reach WP Codebox exactly as written, with no synthesis');
    assert.ok(!(options.mounts || []).some((entry) => /wp-codebox-managed-phpunit-default\.xml$/.test(entry.target)), 'no synthesis mount may be added when a config is already declared');
  }

  // --- explicit wp_codebox_phpunit_suites is completely unaffected ---------
  {
    const { run, capturedOptions } = execute('explicit-suites', explicitConfig, {
      wp_codebox_phpunit_suites: [{ name: 'only', config: 'phpunit-custom.xml.dist' }],
    });
    assert.equal(run.status, 0, `expected declared suites to keep passing, got ${run.status}\n${run.stdout}\n${run.stderr}`);
    const options = JSON.parse(await readFile(capturedOptions, 'utf8'));
    assert.equal(options.phpunitXml, 'phpunit-custom.xml.dist');
    assert.ok(!(options.mounts || []).some((entry) => /wp-codebox-managed-phpunit-default\.xml$/.test(entry.target)), 'no synthesis mount may be added for an explicitly declared suite');
  }

  // --- non-managed bootstrap is unaffected by the absence of a config ------
  // 'project' mode means the component's own bootstrap decides what runs;
  // synthesizing a config here would silently override that declaration.
  // Whatever else 'project' mode does with an empty config is out of scope
  // here -- only that this synthesis never touches it.
  {
    const { capturedOptions } = execute('project-mode', withTests, { wp_codebox_phpunit_bootstrap_mode: 'project' });
    const options = JSON.parse(await readFile(capturedOptions, 'utf8'));
    assert.equal(options.phpunitXml, undefined, 'a non-managed bootstrap mode must reach WP Codebox with its original empty config, not a synthesized one');
    assert.ok(!(options.mounts || []).some((entry) => /wp-codebox-managed-phpunit-default\.xml$/.test(entry.target)), 'no synthesis mount may be added outside managed bootstrap');
  }
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit managed default suite smoke passed.');
