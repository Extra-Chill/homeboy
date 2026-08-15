/**
 * Regression: Extra-Chill/homeboy#11992. When the recipe-run ledger records only
 * setup steps (composer install, plugin activation) and the PHPUnit step never
 * executes, the run must name the stage that stopped before test execution
 * instead of reporting a silently setup-only "zero tests" run. The recipe-run
 * step ledger is preserved as structured evidence, the diagnosis derives a
 * specific ledger cause, and the retained log carries an explicit banner.
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
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-recipe-run-ledger-'));
const component = path.join(root, 'sample-plugin');
const cli = path.join(root, 'wp-codebox');
await mkdir(component, { recursive: true });
await writeFile(path.join(component, 'sample-plugin.php'), '<?php\n/* Plugin Name: Sample Plugin */\n');

// The stub stands in for WP Codebox: composer install and plugin activation run
// cleanly, the PHPUnit step is absent from the ledger, and recipe-run exits
// non-zero because the recipe never executed PHPUnit.
await writeFile(cli, `#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.20.0'); process.exit(0); }
if (args[0] === 'recipe' && args[1] === 'build') {
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
const artifactRoot = args[args.indexOf('--artifacts') + 1];
const runtime = path.join(artifactRoot, 'runtime-fixture');
fs.mkdirSync(path.join(runtime, 'files'), { recursive: true });
fs.writeFileSync(path.join(artifactRoot, 'latest-runtime.json'), JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
const fixture = JSON.parse(process.env.RECIPE_RUN_FIXTURE || '{}');
fs.writeFileSync(path.join(runtime, 'files', 'test-results.json'), JSON.stringify(fixture.testResults || {
  schema: 'wp-codebox/test-results/v1',
  status: 'skipped',
  summary: { total: 0, passed: 0, failed: 0, skipped: 0, unknown: 0 },
  suites: [],
}));
if (typeof fixture.phpunitOutput === 'string') {
  fs.writeFileSync(path.join(runtime, 'files', 'phpunit-output.log'), fixture.phpunitOutput);
}
if (typeof fixture.stageLog === 'string') {
  fs.mkdirSync(path.join(runtime, 'files', 'phpunit'), { recursive: true });
  fs.writeFileSync(path.join(runtime, 'files', 'phpunit', '.pg-test-result.txt'), fixture.stageLog);
}
if (typeof fixture.commandsLog === 'string') {
  fs.mkdirSync(path.join(runtime, 'logs'), { recursive: true });
  fs.writeFileSync(path.join(runtime, 'logs', 'commands.log'), fixture.commandsLog);
}
process.stdout.write(JSON.stringify({ success: false, executions: fixture.executions || [] }) + '\\n');
process.exitCode = fixture.exitCode === undefined ? 1 : fixture.exitCode;
`);
await chmod(cli, 0o755);

async function runScenario(name, fixture) {
  const artifacts = path.join(root, `${name}-artifacts`);
  const invocationArtifacts = path.join(root, `${name}-invocation`);
  await mkdir(invocationArtifacts, { recursive: true });
  await writeFile(path.join(invocationArtifacts, 'homeboy-artifact-manifest.json'), '{"schema":"homeboy/artifact-manifest/v1"}\n');
  const run = spawnSync(runner, [], {
    env: {
      ...process.env,
      RECIPE_RUN_FIXTURE: JSON.stringify(fixture),
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'sample-plugin',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts,
      HOMEBOY_INVOCATION_ARTIFACT_DIR: invocationArtifacts,
      HOMEBOY_SETTINGS_JSON: '{}',
      FIXTURE_SECRET_TOKEN: fixture.secretValue || '',
    },
    encoding: 'utf8',
    maxBuffer: 2 * 1024 * 1024,
  });
  const publishedFiles = path.join(invocationArtifacts, 'wp-codebox-phpunit', 'files');
  return { run, publishedFiles, invocationArtifacts };
}

try {
  const setupOnlyFixture = {
    executions: [
      {
        command: 'composer.install',
        status: 'completed',
        exitCode: 0,
        stdout: 'Installing phpunit/phpunit (9.6.34): Extracting archive\nGenerating autoload files\n',
        stderr: '',
      },
      {
        command: 'wordpress.activate',
        status: 'completed',
        exitCode: 0,
        stdout: '{"activated":["data-machine","sample-plugin"]}\n',
        stderr: '',
      },
    ],
  };

  const setupOnly = await runScenario('setup-only', setupOnlyFixture);
  assert.equal(setupOnly.run.status, 1, setupOnly.run.stderr);
  assert.match(setupOnly.run.stdout, /PHPUNIT_ZERO_TESTS cause=phpunit_step_not_executed/);

  const diagnosis = JSON.parse(await readFile(path.join(setupOnly.publishedFiles, 'phpunit-execution-diagnosis.json'), 'utf8'));
  assert.equal(diagnosis.schema, 'homeboy/wordpress-phpunit-execution-diagnosis/v1');
  assert.equal(diagnosis.executed_tests, 0);
  assert.equal(diagnosis.cause, 'phpunit_step_not_executed', `unexpected zero-test cause: ${diagnosis.cause}`);
  assert.notEqual(diagnosis.cause, 'bootstrap_evidence_unavailable');
  assert.notEqual(diagnosis.cause, 'suite_reported_no_tests');
  assert.ok(diagnosis.evidence.some((entry) => entry.kind === 'recipe-run-steps' && entry.uri === 'artifact://files/recipe-run-steps.json'));

  // The recipe-run step ledger is preserved as first-class structured evidence.
  const ledger = JSON.parse(await readFile(path.join(setupOnly.publishedFiles, 'recipe-run-steps.json'), 'utf8'));
  assert.equal(ledger.schema, 'homeboy/wordpress-recipe-run-steps/v1');
  assert.equal(ledger.parse_status, 'executions');
  assert.equal(ledger.phpunit_executed, false);
  assert.deepEqual(ledger.phpunit_step_indexes, []);
  assert.deepEqual(ledger.executions.map(({ command, exit_code, stdout_bytes, phpunit }) => [command, exit_code, stdout_bytes, phpunit]), [
    ['composer.install', 0, 82, false],
    ['wordpress.activate', 0, 47, false],
  ]);

  const manifest = JSON.parse(await readFile(path.join(setupOnly.invocationArtifacts, 'homeboy-artifact-manifest.json'), 'utf8'));
  const registered = manifest.artifacts.map((artifact) => artifact.path);
  assert.ok(registered.includes('wp-codebox-phpunit/files/recipe-run-steps.json'), `ledger not registered: ${registered.join(', ')}`);
  assert.ok(registered.includes('wp-codebox-phpunit/files/phpunit-output.log'), `output not registered: ${registered.join(', ')}`);

  // The retained log makes the missing PHPUnit step explicit and greppable.
  const output = await readFile(path.join(setupOnly.publishedFiles, 'phpunit-output.log'), 'utf8');
  assert.match(output, /WP_CODEBOX_RECIPE_RUN: NO_PHPUNIT_EXECUTION/);
  assert.match(output, /composer\.install/);
  assert.match(output, /Installing phpunit\/phpunit \(9\.6\.34\)/);

  // The run still resolves to a failing status; a friendlier diagnosis does not
  // turn a red run green.
  const results = JSON.parse(await readFile(path.join(setupOnly.publishedFiles, 'test-results.json'), 'utf8'));
  assert.equal(results.status, 'failed');
  assert.ok(results.evidenceReferences.some((entry) => entry.kind === 'recipe-run-steps' && entry.uri === 'artifact://files/recipe-run-steps.json'));

  // PHPUnit can execute inside the sandbox without appearing as a top-level
  // recipe execution. Preserve its authoritative artifact instead of replacing
  // the failure trace with a false no-execution banner.
  const structuredFailure = await runScenario('structured-failure', {
    executions: setupOnlyFixture.executions,
    phpunitOutput: 'There was 1 failure:\n1) SampleTest::test_failure\nsecret=fixture-phpunit-secret\nFailed asserting that false is true.\nTests: 23, Assertions: 40, Failures: 1.\n',
    secretValue: 'fixture-phpunit-secret',
    stageLog: 'PLUGIN_DETECTED sample-plugin/sample-plugin.php\nSCOPED_TEST_FILES requested=1 matched=1\nDISCOVERY: found=1\n',
    testResults: {
      schema: 'wp-codebox/test-results/v1',
      status: 'failed',
      summary: { total: 23, passed: 22, failed: 1, skipped: 0, unknown: 0 },
      suites: [],
      rawLogReferences: [{ path: 'files/phpunit-output.log', kind: 'phpunit-output' }],
    },
  });
  assert.equal(structuredFailure.run.status, 1, structuredFailure.run.stderr);
  const structuredOutput = await readFile(path.join(structuredFailure.publishedFiles, 'phpunit-output.log'), 'utf8');
  assert.match(structuredOutput, /SampleTest::test_failure/);
  assert.doesNotMatch(structuredOutput, /NO_PHPUNIT_EXECUTION/);
  assert.doesNotMatch(structuredOutput, /fixture-phpunit-secret/);
  assert.match(structuredOutput, /secret=\[REDACTED\]/);
  const structuredDiagnosis = JSON.parse(await readFile(path.join(structuredFailure.publishedFiles, 'phpunit-execution-diagnosis.json'), 'utf8'));
  assert.equal(structuredDiagnosis.executed_tests, 23);
  assert.equal(structuredDiagnosis.cause, 'tests_executed');

  // WP Codebox's real failed-suite artifact shape stores stage markers in the
  // phpunit diagnostic and TextUI's failure trace in the referenced command log.
  const referencedCommandLog = await runScenario('referenced-command-log', {
    executions: setupOnlyFixture.executions,
    stageLog: 'STAGE_BEGIN:run_tests\nRUNNING 1 TEST FILES\n',
    commandsLog: [
      '[2026-08-14T00:00:00.000Z] wordpress.phpunit plugin-slug=sample-plugin',
      'exitCode=1',
      'PHPUnit 9.6.34 by Sebastian Bergmann and contributors.',
      '',
      'There were 4 failures:',
      '',
      '1) SampleTest::test_first',
      'Failed asserting that false is true.',
      '',
      '2) SampleTest::test_second',
      'Failed asserting that null is not null.',
      '',
      '3) SampleTest::test_third',
      'Failed asserting that two strings are identical.',
      '',
      '4) SampleTest::test_fourth',
      'Failed asserting that an array has the key expected.',
      '',
      'FAILURES!',
      'Tests: 23, Assertions: 40, Failures: 4.',
      '',
    ].join('\n'),
    testResults: {
      schema: 'wp-codebox/test-results/v1',
      status: 'failed',
      summary: { total: 23, passed: 19, failed: 4, skipped: 0, unknown: 0 },
      suites: [],
      rawLogReferences: [
        { path: 'logs/commands.log', kind: 'commands-log' },
        { path: 'files/phpunit/.pg-test-result.txt', kind: 'phpunit-output' },
      ],
    },
  });
  const referencedOutput = await readFile(path.join(referencedCommandLog.publishedFiles, 'phpunit-output.log'), 'utf8');
  assert.match(referencedOutput, /SampleTest::test_first/);
  assert.match(referencedOutput, /SampleTest::test_fourth/);
  assert.match(referencedOutput, /Tests: 23, Assertions: 40, Failures: 4/);
  assert.doesNotMatch(referencedOutput, /NO_PHPUNIT_EXECUTION/);
  const referencedFailures = JSON.parse(await readFile(path.join(referencedCommandLog.publishedFiles, 'test-failures.json'), 'utf8'));
  assert.equal(referencedFailures.failures.length, 4);

  const staleSkippedCommandLog = await runScenario('stale-skipped-command-log', {
    executions: setupOnlyFixture.executions,
    stageLog: 'STAGE_BEGIN:run_tests\nRUNNING 1 TEST FILES\n',
    commandsLog: '[2026-08-14T00:00:00.000Z] wordpress.phpunit\nexitCode=1\n1) StaleTest::test_failure\nTests: 23, Assertions: 40, Failures: 4.\n',
    testResults: {
      schema: 'wp-codebox/test-results/v1',
      status: 'failed',
      summary: { total: 23, passed: 18, failed: 4, skipped: 1, unknown: 0 },
      suites: [],
      rawLogReferences: [{ path: 'logs/commands.log', kind: 'commands-log' }],
    },
  });
  const staleSkippedOutput = await readFile(path.join(staleSkippedCommandLog.publishedFiles, 'phpunit-output.log'), 'utf8');
  assert.doesNotMatch(staleSkippedOutput, /StaleTest::test_failure/);
  const staleSkippedResults = JSON.parse(await readFile(path.join(staleSkippedCommandLog.publishedFiles, 'test-results.json'), 'utf8'));
  assert.deepEqual(staleSkippedResults.summary, { total: 23, passed: 18, failed: 4, skipped: 1, unknown: 0 });

  const staleSkippedPreserved = await runScenario('stale-skipped-preserved-output', {
    executions: setupOnlyFixture.executions,
    phpunitOutput: '1) StalePreservedTest::test_failure\nTests: 23, Assertions: 40, Failures: 4.\n',
    stageLog: 'STAGE_BEGIN:run_tests\nRUNNING 1 TEST FILES\n',
    testResults: {
      schema: 'wp-codebox/test-results/v1',
      status: 'failed',
      summary: { total: 23, passed: 18, failed: 4, skipped: 1, unknown: 0 },
      suites: [],
      rawLogReferences: [{ path: 'files/phpunit-output.log', kind: 'phpunit-output' }],
    },
  });
  const staleSkippedPreservedOutput = await readFile(path.join(staleSkippedPreserved.publishedFiles, 'phpunit-output.log'), 'utf8');
  assert.doesNotMatch(staleSkippedPreservedOutput, /StalePreservedTest::test_failure/);

  const multiCommandLog = await runScenario('multi-command-log', {
    executions: setupOnlyFixture.executions,
    stageLog: 'STAGE_BEGIN:run_tests\nRUNNING 2 TEST FILES\n',
    commandsLog: [
      '[2026-08-14T00:00:00.000Z] wordpress.phpunit process-identity=one',
      'exitCode=1',
      'There was 1 failure:',
      '',
      '1) FirstTest::test_failure',
      'Failed asserting that false is true.',
      'FAILURES!',
      'Tests: 10, Assertions: 15, Failures: 1.',
      '---',
      '[2026-08-14T00:00:01.000Z] wordpress.phpunit process-identity=two',
      'exitCode=1',
      'There was 1 failure:',
      '',
      '1) SecondTest::test_failure',
      'Failed asserting that null is not null.',
      'FAILURES!',
      'Tests: 13, Assertions: 25, Failures: 1.',
      '',
    ].join('\n'),
    testResults: {
      schema: 'wp-codebox/test-results/v1',
      status: 'failed',
      summary: { total: 23, passed: 21, failed: 2, skipped: 0, unknown: 0 },
      suites: [],
      rawLogReferences: [{ path: 'logs/commands.log', kind: 'commands-log' }],
    },
  });
  const multiOutput = await readFile(path.join(multiCommandLog.publishedFiles, 'phpunit-output.log'), 'utf8');
  assert.match(multiOutput, /FirstTest::test_failure/);
  assert.match(multiOutput, /SecondTest::test_failure/);
  const multiResults = JSON.parse(await readFile(path.join(multiCommandLog.publishedFiles, 'test-results.json'), 'utf8'));
  assert.deepEqual(multiResults.summary, { total: 23, passed: 21, failed: 2, skipped: 0, unknown: 0 });
  const multiFailures = JSON.parse(await readFile(path.join(multiCommandLog.publishedFiles, 'test-failures.json'), 'utf8'));
  assert.equal(multiFailures.failures.length, 2);

  const staleOutput = await runScenario('stale-output', {
    executions: setupOnlyFixture.executions,
    phpunitOutput: 'OK (99 tests, 99 assertions)\n',
  });
  const staleRetainedOutput = await readFile(path.join(staleOutput.publishedFiles, 'phpunit-output.log'), 'utf8');
  assert.match(staleRetainedOutput, /NO_PHPUNIT_EXECUTION/);
  assert.doesNotMatch(staleRetainedOutput, /OK \(99 tests/);

  const bootstrapFailure = await runScenario('bootstrap-failure-with-stale-output', {
    executions: setupOnlyFixture.executions,
    phpunitOutput: 'OK (99 tests, 99 assertions)\n',
    stageLog: 'STAGE_FAIL:activation: plugin activation failed\n',
    testResults: {
      schema: 'wp-codebox/test-results/v1',
      status: 'passed',
      summary: { total: 99, passed: 99, failed: 0, skipped: 0, unknown: 0 },
      suites: [],
    },
  });
  const bootstrapOutput = await readFile(path.join(bootstrapFailure.publishedFiles, 'phpunit-output.log'), 'utf8');
  assert.doesNotMatch(bootstrapOutput, /OK \(99 tests/);
  const bootstrapResults = JSON.parse(await readFile(path.join(bootstrapFailure.publishedFiles, 'test-results.json'), 'utf8'));
  assert.equal(bootstrapResults.status, 'failed');
  assert.equal(bootstrapResults.summary.total, 0);

  const structuredWithoutRawOutput = await runScenario('structured-without-raw-output', {
    executions: [],
    testResults: {
      schema: 'wp-codebox/test-results/v1',
      status: 'failed',
      summary: { total: 3, passed: 2, failed: 1, skipped: 0, unknown: 0 },
      suites: [],
    },
  });
  const unavailableOutput = await readFile(path.join(structuredWithoutRawOutput.publishedFiles, 'phpunit-output.log'), 'utf8');
  assert.match(unavailableOutput, /WP_CODEBOX_PHPUNIT_OUTPUT: UNAVAILABLE/);
  assert.doesNotMatch(unavailableOutput, /NO_PHPUNIT_EXECUTION/);

  const stalePositiveOutput = await runScenario('stale-positive-output', {
    executions: setupOnlyFixture.executions,
    phpunitOutput: 'OK (99 tests, 99 assertions)\n',
    testResults: {
      schema: 'wp-codebox/test-results/v1',
      status: 'failed',
      summary: { total: 3, passed: 2, failed: 1, skipped: 0, unknown: 0 },
      suites: [],
    },
  });
  const stalePositiveRetained = await readFile(path.join(stalePositiveOutput.publishedFiles, 'phpunit-output.log'), 'utf8');
  assert.doesNotMatch(stalePositiveRetained, /OK \(99 tests/);
  const stalePositiveResults = JSON.parse(await readFile(path.join(stalePositiveOutput.publishedFiles, 'test-results.json'), 'utf8'));
  assert.equal(stalePositiveResults.summary.total, 3);
  assert.equal(stalePositiveResults.summary.failed, 1);

  // A recipe step that exited non-zero names the step and its exit code ahead
  // of the generic missing-PHPUnit cause.
  const activationFailedFixture = {
    executions: [
      { command: 'composer.install', status: 'completed', exitCode: 0, stdout: 'Generating autoload files\n', stderr: '' },
      { command: 'wordpress.activate', status: 'failed', exitCode: 5, stdout: '', stderr: 'Fatal: unable to activate sample-plugin\n' },
    ],
  };
  const activationFailed = await runScenario('activation-failed', activationFailedFixture);
  assert.equal(activationFailed.run.status, 1, activationFailed.run.stderr);
  const failedDiagnosis = JSON.parse(await readFile(path.join(activationFailed.publishedFiles, 'phpunit-execution-diagnosis.json'), 'utf8'));
  assert.equal(failedDiagnosis.cause, 'recipe_step_failed', `unexpected cause: ${failedDiagnosis.cause}`);
  assert.match(failedDiagnosis.detail, /wordpress\.activate/);
  assert.match(failedDiagnosis.detail, /exited with code 5/);
  assert.notEqual(failedDiagnosis.cause, 'phpunit_step_not_executed');
  const failedLedger = JSON.parse(await readFile(path.join(activationFailed.publishedFiles, 'recipe-run-steps.json'), 'utf8'));
  assert.equal(failedLedger.executions[1].exit_code, 5);
  assert.match(await readFile(path.join(activationFailed.publishedFiles, 'phpunit-output.log'), 'utf8'), /WP_CODEBOX_RECIPE_RUN: NO_PHPUNIT_EXECUTION/);

  // Ordering: the ledger cause is INFERRED, so a stage marker that names the
  // seam directly must win. The bootstrap legitimately skips the PHPUnit
  // invocation when the changed-file scope matched nothing, which leaves the
  // ledger with no PHPUnit step — reporting the generic "no step invoked
  // PHPUnit" there would mask the precise reason the run selected no tests.
  const scopeMismatch = await runScenario('scope-mismatch', {
    executions: setupOnlyFixture.executions,
    stageLog: [
      'PLUGIN_DETECTED sample-plugin/sample-plugin.php',
      'PLUGIN_ACTIVATE_OK sample-plugin/sample-plugin.php',
      'DISCOVERY: found=12',
      'SCOPED_TEST_FILES requested=7 matched=0',
    ].join('\n'),
  });
  const scopeDiagnosis = JSON.parse(await readFile(path.join(scopeMismatch.publishedFiles, 'phpunit-execution-diagnosis.json'), 'utf8'));
  assert.equal(scopeDiagnosis.cause, 'changed_file_filter_mismatch', `stage marker must outrank the inferred ledger cause, got: ${scopeDiagnosis.cause}`);
  // The ledger is still preserved even when a stage marker supplies the cause.
  const scopeLedger = JSON.parse(await readFile(path.join(scopeMismatch.publishedFiles, 'recipe-run-steps.json'), 'utf8'));
  assert.equal(scopeLedger.phpunit_executed, false);

  // Same ordering rule for empty discovery.
  const discoveryEmpty = await runScenario('discovery-empty', {
    executions: setupOnlyFixture.executions,
    stageLog: [
      'PLUGIN_DETECTED sample-plugin/sample-plugin.php',
      'PLUGIN_ACTIVATE_OK sample-plugin/sample-plugin.php',
      'NO_TEST_FILES',
    ].join('\n'),
  });
  const discoveryDiagnosis = JSON.parse(await readFile(path.join(discoveryEmpty.publishedFiles, 'phpunit-execution-diagnosis.json'), 'utf8'));
  assert.equal(discoveryDiagnosis.cause, 'phpunit_discovery_empty', `stage marker must outrank the inferred ledger cause, got: ${discoveryDiagnosis.cause}`);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit recipe-run ledger smoke passed.');
