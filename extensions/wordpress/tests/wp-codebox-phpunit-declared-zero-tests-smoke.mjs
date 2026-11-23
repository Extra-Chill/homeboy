/**
 * Regression: a sandbox suite whose own PHPUnit config declares test
 * directories holding test files must not report a zero-test run as a pass.
 *
 * `phpunit_no_tests` defaults to "skipped", which is right for a component that
 * genuinely has nothing to run. It is wrong for a suite that declares
 * tests/ with *Test.php files in it and then executes none of them (for
 * example a sandbox whose WordPress source never resolved): that zero hides
 * the failure. Such a run now fails with PHPUNIT_ZERO_TESTS_FAILED unless the
 * component explicitly sets `phpunit_no_tests`.
 *
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-declared-zero-'));
const cli = path.join(root, 'wp-codebox');

async function makeComponent(name, { withTests }) {
  const component = path.join(root, name);
  await mkdir(path.join(component, 'tests/Unit'), { recursive: true });
  await writeFile(path.join(component, `${name}.php`), `<?php\n/* Plugin Name: ${name} */\n`);
  if (withTests) {
    await writeFile(path.join(component, 'tests/Unit/FirstTest.php'), '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class FirstTest extends TestCase { public function test_it(): void { $this->assertTrue(true); } }\n');
  } else {
    await writeFile(path.join(component, 'tests/Unit/helper.php'), '<?php\n');
  }
  await writeFile(path.join(component, 'phpunit.xml.dist'), '<?xml version="1.0"?>\n<phpunit><testsuites><testsuite name="s"><directory>tests</directory></testsuite></testsuites></phpunit>\n');
  return component;
}

// The fake CLI reports a run that executed zero tests.
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
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
const artifactRoot = args[args.indexOf('--artifacts') + 1];
const runtime = path.join(artifactRoot, 'runtime-fixture');
fs.mkdirSync(path.join(runtime, 'files', 'phpunit'), { recursive: true });
fs.writeFileSync(path.join(artifactRoot, 'latest-runtime.json'), JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
fs.writeFileSync(path.join(runtime, 'files', 'test-results.json'), JSON.stringify({
  schema: 'wp-codebox/test-results/v1',
  status: 'unknown',
  summary: { total: 0, passed: 0, failed: 0, skipped: 0, unknown: 0 },
  suites: [],
}));
process.stdout.write(JSON.stringify({
  success: true,
  executions: [{ command: 'wordpress.phpunit', exitCode: 0, stdout: 'No tests executed!\\n', stderr: '' }],
}));
`);
await chmod(cli, 0o755);

async function execute(name, component, settings) {
  const artifacts = path.join(root, `${name}-artifacts`);
  const run = spawnSync('bash', [runner], {
    env: {
      ...process.env,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: path.basename(component),
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts,
      HOMEBOY_SETTINGS_JSON: JSON.stringify({ wp_codebox_phpunit_bootstrap_mode: 'managed', ...settings }),
    },
    encoding: 'utf8',
  });
  const runDirectory = path.join(artifacts, (await readdir(artifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.')));
  const results = JSON.parse(await readFile(path.join(runDirectory, 'runtime-fixture', 'files', 'test-results.json'), 'utf8'));
  return { run, results };
}

try {
  const declared = await makeComponent('declared-plugin', { withTests: true });
  const empty = await makeComponent('empty-plugin', { withTests: false });

  // Default policy + declared test files + zero executed: fail, and say why.
  {
    const { run, results } = await execute('declared-default', declared, {});
    assert.notEqual(run.status, 0, `declared suite with zero tests must fail\n${run.stdout}\n${run.stderr}`);
    assert.equal(results.status, 'failed');
    assert.match(run.stdout, /PHPUNIT_ZERO_TESTS_FAILED: suite "default" declares 1 test file\(s\) under tests but PHPUnit executed 0 tests/);
  }

  // An explicit phpunit_no_tests setting is respected.
  {
    const { run, results } = await execute('declared-explicit-skip', declared, { phpunit_no_tests: 'skipped' });
    assert.equal(run.status, 0, `explicit phpunit_no_tests=skipped must allow an empty run\n${run.stdout}\n${run.stderr}`);
    assert.equal(results.status, 'skipped');
    assert.doesNotMatch(run.stdout, /PHPUNIT_ZERO_TESTS_FAILED/);
  }
  {
    const { run, results } = await execute('declared-explicit-fail', declared, { phpunit_no_tests: 'fail' });
    assert.notEqual(run.status, 0);
    assert.equal(results.status, 'failed');
  }

  // Declared directories with no test files keep the default skip.
  {
    const { run, results } = await execute('empty-default', empty, {});
    assert.equal(run.status, 0, `a suite whose directories hold no test files keeps the default skip\n${run.stdout}\n${run.stderr}`);
    assert.equal(results.status, 'skipped');
    assert.doesNotMatch(run.stdout, /PHPUNIT_ZERO_TESTS_FAILED/);
  }

  console.log('ok: declared PHPUnit suites fail on zero executed tests unless phpunit_no_tests is explicit');
} finally {
  await rm(root, { recursive: true, force: true });
}
