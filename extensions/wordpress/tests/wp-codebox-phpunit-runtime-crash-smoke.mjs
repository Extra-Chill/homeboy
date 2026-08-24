/**
 * Regression: Extra-Chill/homeboy#12617. A PHP-WASM trap leaves the interpreter
 * in an undefined state, but the WP Codebox CLI logs the rejection instead of
 * exiting, so the recipe-run promise never settles. Before this, a crash that
 * happened in the first seconds cost the full test budget — 24 minutes per
 * shard, four shards deep — and surfaced only as "test phase timed out ...
 * before reporting test counts", naming the symptom and hiding the cause.
 *
 * The run must now terminate on the crash signature, and report the crash as
 * its failure cause. A run without the signature must be left alone.
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
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-runtime-crash-'));
const component = path.join(root, 'sample-plugin');
const cli = path.join(root, 'wp-codebox');
await mkdir(component, { recursive: true });
await writeFile(path.join(component, 'sample-plugin.php'), '<?php\n/* Plugin Name: Sample Plugin */\n');

// The budget is what the fix must avoid paying; the grace is what it should pay
// instead. Keeping them far apart is what makes the assertion meaningful while
// the test still runs in seconds.
const BUDGET_SECONDS = 90;
const GRACE_SECONDS = 2;

// Stands in for WP Codebox wedged by a wasm trap: it emits the crash the way
// the runtime does — an unhandled rejection with a php.wasm stack — completes
// its setup step, and then never returns.
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
process.stdout.write('Running the wordpress.run-php setup step...\\n');
if (process.env.FIXTURE_EMIT_CRASH === '1') {
  process.stderr.write([
    'Unhandled rejection: RuntimeError: null function or function signature mismatch',
    '    at php.wasm._php_stream_write_filtered (wasm://wasm/php.wasm-05996276:wasm-function[3039]:0x26c31e)',
    '    at php.wasm.zif_mysqli_poll (wasm://wasm/php.wasm-05996276:wasm-function[12986]:0x9949a8)',
    '',
  ].join('\\n'));
}
if (process.env.FIXTURE_HANG === '1') {
  // The wedged shape: the promise never settles and nothing more is written.
  setInterval(() => {}, 1000);
} else {
  process.stdout.write(JSON.stringify({ success: false, executions: [
    { command: 'wordpress.run-php', status: 'completed', exitCode: 0, stdout: '', stderr: '' },
  ] }) + '\\n');
  process.exit(1);
}
`);
await chmod(cli, 0o755);

async function runScenario(name, fixtureEnv) {
  const artifacts = path.join(root, `${name}-artifacts`);
  const invocationArtifacts = path.join(root, `${name}-invocation`);
  await mkdir(invocationArtifacts, { recursive: true });
  await writeFile(path.join(invocationArtifacts, 'homeboy-artifact-manifest.json'), '{"schema":"homeboy/artifact-manifest/v1"}\n');
  const startedAt = Date.now();
  const run = spawnSync(runner, [], {
    env: {
      ...process.env,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'sample-plugin',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts,
      HOMEBOY_INVOCATION_ARTIFACT_DIR: invocationArtifacts,
      HOMEBOY_SETTINGS_JSON: '{}',
      HOMEBOY_WORDPRESS_PHPUNIT_TIMEOUT_SECONDS: String(BUDGET_SECONDS),
      HOMEBOY_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS: String(GRACE_SECONDS),
      ...fixtureEnv,
    },
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
  });
  return {
    run,
    elapsedSeconds: (Date.now() - startedAt) / 1000,
    publishedFiles: path.join(invocationArtifacts, 'wp-codebox-phpunit', 'files'),
  };
}

try {
  const crashed = await runScenario('runtime-crash', { FIXTURE_EMIT_CRASH: '1', FIXTURE_HANG: '1' });

  // The point of the fix: a wedged runtime costs the grace, not the budget.
  assert.ok(
    crashed.elapsedSeconds < BUDGET_SECONDS / 2,
    `expected the crash to terminate near the ${GRACE_SECONDS}s grace, took ${crashed.elapsedSeconds}s of a ${BUDGET_SECONDS}s budget`
  );
  assert.match(crashed.run.stdout, /WP Codebox runtime crash detected \(php_wasm_unhandled_rejection\)/);
  assert.match(crashed.run.stdout, /PHPUNIT_ZERO_TESTS cause=runtime_crashed/);

  const diagnosis = JSON.parse(await readFile(path.join(crashed.publishedFiles, 'phpunit-execution-diagnosis.json'), 'utf8'));
  assert.equal(diagnosis.executed_tests, 0);
  assert.equal(diagnosis.cause, 'runtime_crashed', `unexpected zero-test cause: ${diagnosis.cause}`);
  assert.match(diagnosis.detail, /null function or function signature mismatch/);
  assert.match(diagnosis.remediation, /PHP-WASM trap/);
  // The ledger cause must lose: with a trapped runtime it describes the
  // wreckage, not what stopped the run.
  assert.notEqual(diagnosis.cause, 'phpunit_step_not_executed');

  const timeoutDiagnostics = JSON.parse(await readFile(path.join(crashed.publishedFiles, 'wp-codebox-timeout-diagnostics.json'), 'utf8'));
  assert.equal(timeoutDiagnostics.termination.result, 'runtime_crash', 'termination must name the crash, not the budget');
  assert.equal(timeoutDiagnostics.runtime_crash.id, 'php_wasm_unhandled_rejection');
  assert.equal(timeoutDiagnostics.runtime_crash.wasm_frame, true);
  assert.match(timeoutDiagnostics.runtime_crash.message, /RuntimeError: null function or function signature mismatch/);

  // Control: no crash signature, so nothing is terminated early and the run
  // keeps its own ledger-derived cause.
  const clean = await runScenario('no-crash', { FIXTURE_EMIT_CRASH: '0', FIXTURE_HANG: '0' });
  assert.doesNotMatch(clean.run.stdout, /WP Codebox runtime crash detected/);
  assert.doesNotMatch(clean.run.stdout, /PHPUNIT_ZERO_TESTS cause=runtime_crashed/);
  const cleanDiagnosis = JSON.parse(await readFile(path.join(clean.publishedFiles, 'phpunit-execution-diagnosis.json'), 'utf8'));
  assert.notEqual(cleanDiagnosis.cause, 'runtime_crashed');

  // A crash signature on a run that still finishes on its own must not be
  // pre-empted: the grace window is the whole safety argument.
  const recovered = await runScenario('crash-then-exit', { FIXTURE_EMIT_CRASH: '1', FIXTURE_HANG: '0' });
  assert.match(recovered.run.stdout, /WP Codebox runtime crash detected/);
  assert.ok(
    recovered.elapsedSeconds < BUDGET_SECONDS / 2,
    `expected a self-terminating run to exit promptly, took ${recovered.elapsedSeconds}s`
  );

  console.log('wp-codebox phpunit runtime crash smoke passed');
} finally {
  await rm(root, { recursive: true, force: true });
}
