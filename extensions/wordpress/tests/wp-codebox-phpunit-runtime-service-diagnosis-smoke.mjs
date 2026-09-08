/**
 * Reproduces homeboy#14426.
 *
 * WP Codebox could not provision the managed MySQL service on a Docker-free
 * host, so the recipe-run died before flushing its JSON summary. The adapter
 * saw only an unparseable payload and reported `recipe_run_payload_unparseable`
 * -- a true statement about the wreckage that masked the `provider-unavailable`
 * lifecycle evidence sitting next to it in the same run directory.
 *
 * The provisioning failure outranks the parse failure because provisioning
 * precedes execution: the payload is unparseable *because* the service never
 * came up.
 *
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-runtime-service-diagnosis-'));
const component = path.join(root, 'sample-plugin');
const artifacts = path.join(root, 'artifacts');
const cli = path.join(root, 'wp-codebox');
const resultsFile = path.join(root, 'test-results.json');
const failuresFile = path.join(root, 'test-failures.json');
const writeResults = path.join(root, 'write-test-results.sh');
const extension = path.resolve(import.meta.dirname, '..');

await mkdir(component, { recursive: true });
await writeFile(path.join(component, 'sample-plugin.php'), '<?php\n/* Plugin Name: Sample Plugin */\n');
await writeFile(writeResults, `homeboy_write_test_results() { printf '{"total":%s,"passed":%s,"failed":%s,"skipped":%s}\n' "$1" "$2" "$3" "$4" > "$HOMEBOY_TEST_RESULTS_FILE"; }
`);

// Mirrors the upstream failure: `wp-codebox/recipe-run-artifact-pointer/v1`
// carries the lifecycle evidence, and stdout carries the provisioning error
// rather than a parseable recipe-run payload.
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
fs.writeFileSync(path.join(artifacts, 'latest-runtime.json'), JSON.stringify({
  schema: 'wp-codebox/recipe-run-artifact-pointer/v1',
  paths: { runtimeDirectory: 'runtime-fixture' },
  managedRuntimeServices: [{
    id: 'wordpress-database',
    kind: 'mysql',
    provider: 'docker',
    version: 'mysql:8.4',
    readiness: 'pending',
    lifecycle: 'failed',
    diagnostic: {
      code: 'provider-unavailable',
      command: 'docker',
      cause: { code: 'ENOENT', message: 'Provider command executable was not found' },
    },
  }],
}));
process.stderr.write('RuntimeServiceProvisionError: Managed runtime service failed: wordpress-database\\n');
process.stdout.write('Managed runtime service provisioning failed before any step ran.\\n');
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
      HOMEBOY_TEST_RESULTS_FILE: resultsFile,
      HOMEBOY_TEST_FAILURES_FILE: failuresFile,
      HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: writeResults,
      HOMEBOY_SETTINGS_JSON: '{}',
    },
    encoding: 'utf8',
    maxBuffer: 2 * 1024 * 1024,
  });

  assert.equal(run.status, 1, run.stderr);

  const runDirectory = (await readdir(artifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.'));
  assert.ok(runDirectory, 'adapter must retain the run artifacts');
  const runtime = path.join(artifacts, runDirectory, 'runtime-fixture');

  const steps = JSON.parse(await readFile(path.join(runtime, 'files/recipe-run-steps.json'), 'utf8'));
  assert.equal(steps.parse_status, 'unparseable', 'fixture must reproduce the masking payload state');

  const diagnosis = JSON.parse(await readFile(path.join(runtime, 'files/phpunit-execution-diagnosis.json'), 'utf8'));
  assert.equal(
    diagnosis.cause,
    'runtime_service_provider_unavailable',
    `provisioning failure must outrank the unparseable payload, got '${diagnosis.cause}'`
  );
  assert.match(diagnosis.detail, /wordpress-database/);
  assert.match(diagnosis.detail, /docker/);
  assert.match(diagnosis.remediation, /managed-runtime-services\.json/);

  // The lifecycle evidence the diagnosis points at must actually be published.
  const services = JSON.parse(await readFile(path.join(runtime, 'files/managed-runtime-services.json'), 'utf8'));
  assert.equal(services[0].diagnostic.code, 'provider-unavailable');

  // The cause is named in the runner's own output, not only in a sidecar, so an
  // operator reading the log does not have to know the artifact exists.
  assert.match(run.stdout, /PHPUNIT_ZERO_TESTS cause=runtime_service_provider_unavailable/);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit runtime service diagnosis smoke passed.');
