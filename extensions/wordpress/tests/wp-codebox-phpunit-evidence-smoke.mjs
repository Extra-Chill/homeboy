/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-evidence-'));
const component = path.join(root, 'sample-plugin');
const artifacts = path.join(root, 'artifacts');
const cli = path.join(root, 'wp-codebox');
const resultsFile = path.join(root, 'test-results.json');
const failuresFile = path.join(root, 'test-failures.json');
const expectedOutputFile = path.join(root, 'expected-output.log');
const invocationArtifacts = path.join(root, 'invocation-artifacts');
const writeResults = path.join(root, 'write-test-results.sh');
const extension = path.resolve(import.meta.dirname, '..');

await mkdir(component, { recursive: true });
await mkdir(invocationArtifacts, { recursive: true });
await writeFile(path.join(invocationArtifacts, 'homeboy-artifact-manifest.json'), '{"schema":"homeboy/artifact-manifest/v1"}\n');
await writeFile(path.join(component, 'sample-plugin.php'), '<?php\n/* Plugin Name: Sample Plugin */\n');
await writeFile(writeResults, `homeboy_write_test_results() { printf '{"total":%s,"passed":%s,"failed":%s,"skipped":%s}\n' "$1" "$2" "$3" "$4" > "$HOMEBOY_TEST_RESULTS_FILE"; }
`);
await writeFile(cli, `#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args.slice(-3).join(' ') === 'runtime descriptor --json') { process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } })); process.exit(0); }
if (args[0] === 'recipe' && args[1] === 'build') {
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
const artifacts = args[args.indexOf('--artifacts') + 1];
const runtime = path.join(artifacts, 'runtime-fixture');
fs.mkdirSync(path.join(runtime, 'files'), { recursive: true });
fs.writeFileSync(path.join(artifacts, 'latest-runtime.json'), JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
fs.writeFileSync(path.join(runtime, 'files', 'test-results.json'), JSON.stringify({
  schema: 'wp-codebox/test-results/v1',
  status: 'failed',
  summary: { total: 3, passed: 0, failed: 2, skipped: 1, unknown: 0 },
  suites: [{ name: 'phpunit', tests: 3, passed: 0, failed: 2, skipped: 1 }],
}));
const output = 'x'.repeat(8192 - 32) + 'fixture-secret-value' + 'x'.repeat(70 * 1024) + '\\n' + [
  'PHPUnit 9.6.22 by Sebastian Bergmann and contributors.',
  'fixture token: fixture-secret-value',
  '',
  'There was 1 error:',
  '',
  '1) SamplePlugin\\Tests\\MixedTest::test_php_error',
  'TypeError: SamplePlugin\\Runner::run(): Return value must be of type string, null returned',
  '${component}/src/Runner.php:18',
  '${component}/tests/MixedTest.php:24',
  '',
  'There was 1 failure:',
  '',
  '1) SamplePlugin\\Tests\\MixedTest::test_assertion_diff',
  'Failed asserting that two strings are identical.',
  '--- Expected',
  '+++ Actual',
  '@@ @@',
  "-'expected'",
  "+'actual'",
  '${component}/tests/MixedTest.php:35',
  '',
  'FAILURES!',
  'Tests: 3, Assertions: 2, Errors: 1, Failures: 1, Skipped: 1.',
  '',
].join('\\n')
  .replaceAll('SamplePluginTestsMixedTest', ['SamplePlugin', 'Tests', 'MixedTest'].join(String.fromCharCode(92)))
  .replaceAll('SamplePluginRunner', ['SamplePlugin', 'Runner'].join(String.fromCharCode(92)));
fs.writeFileSync('${expectedOutputFile}', output);
process.stdout.write(JSON.stringify({ success: false, executions: [{ stdout: output, stderr: '' }] }));
process.exitCode = 1;
`);
await chmod(cli, 0o755);

try {
  const run = spawnSync(path.join(extension, 'scripts/test/test-runner-wp-codebox.sh'), [], {
    env: {
      ...process.env,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'sample-plugin',
      HOMEBOY_EXTENSION_PATH: extension,
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts,
      HOMEBOY_INVOCATION_ARTIFACT_DIR: invocationArtifacts,
      HOMEBOY_TEST_RESULTS_FILE: resultsFile,
      HOMEBOY_TEST_FAILURES_FILE: failuresFile,
      HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: writeResults,
      HOMEBOY_SETTINGS_JSON: '{}',
      PHPUNIT_SECRET_TOKEN: 'fixture-secret-value',
    },
    encoding: 'utf8',
    maxBuffer: 2 * 1024 * 1024,
  });

  assert.equal(run.status, 1, run.stderr);
  assert.match(run.stdout, /Structured PHPUnit evidence: artifact:\/\/files\/test-results\.json/);
  assert.match(run.stdout, /Full PHPUnit output: artifact:\/\/files\/phpunit-output\.log/);

  const runDirectory = (await readdir(artifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.'));
  assert.ok(runDirectory);
  const runtime = path.join(artifacts, runDirectory, 'runtime-fixture');
  const expectedOutput = (await readFile(expectedOutputFile, 'utf8')).replaceAll('fixture-secret-value', '[REDACTED]');
  const retainedOutput = await readFile(path.join(runtime, 'files/phpunit-output.log'), 'utf8');
  assert.ok(Buffer.byteLength(retainedOutput) > 64 * 1024);
  assert.equal(retainedOutput, expectedOutput);
  assert.doesNotMatch(retainedOutput, /fixture-secret-value/);
  assert.match(retainedOutput, /\[REDACTED\]x{100}/);
  assert.match(retainedOutput, /fixture token: \[REDACTED\]/);

  const artifactResults = JSON.parse(await readFile(path.join(runtime, 'files/test-results.json'), 'utf8'));
  const profile = JSON.parse(await readFile(path.join(artifacts, runDirectory, 'wp-codebox-phpunit-profile.json'), 'utf8'));
  assert.equal(profile.phpunit.environment, 'wordpress-integration');
  assert.deepEqual(artifactResults.rawLogReferences.slice(-3), [
    { path: 'files/phpunit-output.log', kind: 'phpunit-output' },
    { path: 'logs/recipe-run.stdout.log', kind: 'recipe-run-stdout' },
    { path: 'logs/recipe-run.stderr.log', kind: 'recipe-run-stderr' },
  ]);
  assert.deepEqual(artifactResults.evidenceReferences, [
    { kind: 'structured-test-results', uri: 'artifact://files/test-results.json' },
    { kind: 'raw-phpunit-output', uri: 'artifact://files/phpunit-output.log' },
    { kind: 'test-execution-diagnosis', uri: 'artifact://files/phpunit-execution-diagnosis.json' },
    { kind: 'recipe-run-steps', uri: 'artifact://files/recipe-run-steps.json' },
  ]);
  assert.deepEqual(JSON.parse(await readFile(resultsFile, 'utf8')), { total: 3, passed: 0, failed: 2, skipped: 1 });
  const invocationManifest = JSON.parse(await readFile(path.join(invocationArtifacts, 'homeboy-artifact-manifest.json'), 'utf8'));
  assert.deepEqual(invocationManifest.artifacts.map(({ path: artifactPath }) => artifactPath), [
    'wp-codebox-phpunit/files/test-results.json',
    'wp-codebox-phpunit/files/phpunit-output.log',
    'wp-codebox-phpunit/files/phpunit-execution-diagnosis.json',
    'wp-codebox-phpunit/files/recipe-run-steps.json',
    'wp-codebox-phpunit/files/test-failures.json',
  ]);
  const publishedDirectory = path.join(invocationArtifacts, 'wp-codebox-phpunit/files');
  assert.equal(await readFile(path.join(publishedDirectory, 'phpunit-output.log'), 'utf8'), expectedOutput);
  assert.deepEqual(JSON.parse(await readFile(path.join(publishedDirectory, 'test-results.json'), 'utf8')), artifactResults);
  const publishedFailures = JSON.parse(await readFile(path.join(publishedDirectory, 'test-failures.json'), 'utf8'));
  assert.equal(publishedFailures.failures.length, 2);
  assert.doesNotMatch(await readFile(path.join(publishedDirectory, 'phpunit-output.log'), 'utf8'), /fixture-secret-value/);

  const analysisInput = JSON.parse(await readFile(failuresFile, 'utf8'));
  assert.equal(analysisInput.total, 3);
  assert.equal(analysisInput.passed, 0);
  assert.deepEqual(analysisInput.metadata, { assertions: 2, failures: 1, errors: 1, skipped: 1 });
  assert.equal(analysisInput.failures.length, 2);
  assert.deepEqual(analysisInput.failures.map(({ status }) => status), ['error', 'failure']);
  assert.equal(analysisInput.failures[0].test_id, 'SamplePlugin\\Tests\\MixedTest::test_php_error');
  assert.equal(analysisInput.failures[0].failure_type, 'TypeError');
  assert.equal(analysisInput.failures[0].file, 'src/Runner.php');
  assert.match(analysisInput.failures[0].stack_trace, /tests\/MixedTest\.php:24/);
  assert.equal(analysisInput.failures[1].test_id, 'SamplePlugin\\Tests\\MixedTest::test_assertion_diff');
  assert.equal(analysisInput.failures[1].failure_type, 'AssertionFailedError');
  assert.match(analysisInput.failures[1].message, /--- Expected\n\+\+\+ Actual/);
  for (const failure of analysisInput.failures) {
    assert.equal(typeof failure.test_id, 'string');
    assert.equal(typeof failure.message, 'string');
    assert.equal(typeof failure.fingerprint, 'string');
    assert.equal(failure.fingerprint.length, 64);
  }
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit evidence smoke passed.');
