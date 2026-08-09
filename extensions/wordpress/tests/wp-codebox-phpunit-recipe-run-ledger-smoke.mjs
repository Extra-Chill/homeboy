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
if (args[0] === 'recipe' && args[1] === 'build') {
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
const artifactRoot = args[args.indexOf('--artifacts') + 1];
const runtime = path.join(artifactRoot, 'runtime-fixture');
fs.mkdirSync(path.join(runtime, 'files'), { recursive: true });
fs.writeFileSync(path.join(artifactRoot, 'latest-runtime.json'), JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
fs.writeFileSync(path.join(runtime, 'files', 'test-results.json'), JSON.stringify({
  schema: 'wp-codebox/test-results/v1',
  status: 'skipped',
  summary: { total: 0, passed: 0, failed: 0, skipped: 0, unknown: 0 },
  suites: [],
}));
const fixture = JSON.parse(process.env.RECIPE_RUN_FIXTURE || '{}');
if (typeof fixture.stageLog === 'string') {
  fs.mkdirSync(path.join(runtime, 'files', 'phpunit'), { recursive: true });
  fs.writeFileSync(path.join(runtime, 'files', 'phpunit', '.pg-test-result.txt'), fixture.stageLog);
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
