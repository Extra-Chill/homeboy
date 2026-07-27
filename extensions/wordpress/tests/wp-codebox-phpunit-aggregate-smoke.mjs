/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-aggregate-'));
const component = path.join(root, 'component');
const dependency = path.join(root, 'dependency');
const dataMachine = path.join(root, 'data-machine');
const conflictingDataMachine = path.join(root, 'conflicting', 'data-machine');
const cli = path.join(root, 'wp-codebox');
const resultsWriter = path.join(root, 'write-results.sh');
await mkdir(path.join(component, 'tests'), { recursive: true });
await mkdir(dependency);
await mkdir(dataMachine);
await mkdir(conflictingDataMachine, { recursive: true });
await writeFile(path.join(component, 'component.php'), '<?php /* Plugin Name: Component */\n');
await writeFile(path.join(component, 'tests', 'bootstrap.php'), '<?php require_once __DIR__ . "/../component.php";\n');
await writeFile(path.join(component, 'phpunit.xml.dist'), '<phpunit bootstrap="tests/bootstrap.php"><testsuites><testsuite name="suite"><directory>tests</directory></testsuite></testsuites></phpunit>\n');
await writeFile(path.join(dependency, 'dependency.php'), '<?php /* Plugin Name: Dependency */\n');
await writeFile(path.join(dataMachine, 'data-machine.php'), '<?php /* Plugin Name: Data Machine */\n');
await writeFile(path.join(conflictingDataMachine, 'data-machine.php'), '<?php /* Plugin Name: Conflicting Data Machine */\n');
await writeFile(resultsWriter, 'homeboy_write_test_results() { printf \'{"total":%s,"passed":%s,"failed":%s,"skipped":%s}\\n\' "$1" "$2" "$3" "$4" > "$HOMEBOY_TEST_RESULTS_FILE"; }\n');
await writeFile(cli, `#!/usr/bin/env node
import fs from 'node:fs';
const args = process.argv.slice(2);
const value = (name) => args[args.indexOf(name) + 1];
if (process.env.DISPATCHED) { fs.appendFileSync(process.env.DISPATCHED, JSON.stringify(args) + '\\n'); }
if (args[0] === 'recipe') { fs.writeFileSync(value('--output'), JSON.stringify({ schema: 'wp-codebox/workspace-recipe/v1' })); process.exit(0); }
const artifacts = value('--artifacts');
const mode = process.env.FIXTURE_MODE;
if (mode !== 'crash') {
  fs.mkdirSync(artifacts + '/runtime-fixture/files', { recursive: true });
  fs.writeFileSync(artifacts + '/latest-runtime.json', JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
  fs.writeFileSync(artifacts + '/runtime-fixture/files/test-results.json', JSON.stringify({ schema: 'wp-codebox/test-results/v1', status: 'unknown', summary: { total: mode === 'failure' ? 281 : 0, passed: 0, failed: 0, skipped: 0 } }));
}
const output = mode === 'success' ? 'OK (3 tests, 76 assertions)\\n' : 'ERRORS!\\nTests: 281, Assertions: 329, Errors: 46, Failures: 100.\\n';
process.stdout.write(JSON.stringify({ executions: [{ stdout: output, stderr: '' }] }));
process.exitCode = mode === 'success' ? 0 : 2;
`);
await chmod(cli, 0o755);

try {
  for (const [mode, expected] of [
    ['failure', { total: 281, passed: 135, failed: 146, skipped: 0 }],
    ['success', { total: 3, passed: 3, failed: 0, skipped: 0 }],
    ['crash', { total: 281, passed: 135, failed: 146, skipped: 0 }],
  ]) {
    const artifacts = path.join(root, `${mode}-artifacts`);
    const results = path.join(root, `${mode}-results.json`);
    const run = spawnSync(runner, [], { env: { ...process.env, FIXTURE_MODE: mode, HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter, HOMEBOY_TEST_RESULTS_FILE: results, HOMEBOY_SETTINGS_JSON: JSON.stringify({ validation_dependencies: [dependency] }) }, encoding: 'utf8' });
    assert.equal(run.status, mode === 'success' ? 0 : 2, run.stderr);
    assert.deepEqual(JSON.parse(await readFile(results, 'utf8')), expected);
    const runArtifact = path.join(artifacts, (await readdir(artifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.')));
    if (mode !== 'crash') {
      const artifactResults = JSON.parse(await readFile(path.join(runArtifact, 'runtime-fixture', 'files', 'test-results.json'), 'utf8'));
      assert.equal(artifactResults.status, expected.failed > 0 ? 'failed' : 'passed');
    }
    const options = JSON.parse(await readFile(path.join(runArtifact, 'wp-codebox-phpunit-recipe-options.json'), 'utf8'));
    const profile = JSON.parse(await readFile(path.join(runArtifact, 'wp-codebox-phpunit-profile.json'), 'utf8'));
    const provenance = JSON.parse(await readFile(path.join(runArtifact, 'wp-codebox-phpunit-provenance.json'), 'utf8'));
    assert.equal(options.extra_plugins[1].activate, true);
    assert.equal(options.phpunitXml, '/wordpress/wp-content/plugins/component/phpunit.xml.dist');
    assert.equal(profile.phpunit.environment, 'standalone-php');
    assert.deepEqual(provenance.source_refs, [
      { slug: 'component', source: component, source_subpath: null },
      { slug: 'dependency', source: dependency },
    ]);
  }
  const missing = spawnSync(runner, [], { env: { ...process.env, HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_SETTINGS_JSON: JSON.stringify({ validation_dependencies: [path.join(root, 'missing')] }) }, encoding: 'utf8' });
  assert.notEqual(missing.status, 0);
  assert.match(missing.stderr, /dependency sources are unavailable/);

  const bin = path.join(root, 'bin');
  const dispatched = path.join(root, 'dispatched.jsonl');
  await mkdir(bin);
  await writeFile(path.join(bin, 'homeboy'), `#!/usr/bin/env sh
printf '%s\\n' '${JSON.stringify({ data: { entity: { local_path: conflictingDataMachine } } })}'
`);
  await chmod(path.join(bin, 'homeboy'), 0o755);
  const metadataArtifacts = path.join(root, 'metadata-artifacts');
  const metadataResults = path.join(root, 'metadata-results.json');
  const metadataRun = spawnSync(runner, [], {
    env: {
      ...process.env,
      PATH: `${bin}:${process.env.PATH}`,
      DISPATCHED: dispatched,
      FIXTURE_MODE: 'success',
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'component',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: metadataArtifacts,
      HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter,
      HOMEBOY_TEST_RESULTS_FILE: metadataResults,
      HOMEBOY_WORDPRESS_DEPENDENCY_PATHS: `${dataMachine}\n${dataMachine}`,
      HOMEBOY_SETTINGS_JSON: JSON.stringify({ validation_dependencies: ['data-machine'] }),
    },
    encoding: 'utf8',
  });
  assert.equal(metadataRun.status, 0, metadataRun.stderr);
  assert.match(metadataRun.stdout, /WP Codebox test run complete\./);
  assert.deepEqual((await readFile(dispatched, 'utf8')).trim().split('\n').map(JSON.parse).map((args) => args[0]), ['recipe', 'recipe-run']);
  const metadataRunArtifact = path.join(metadataArtifacts, (await readdir(metadataArtifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.')));
  const metadataOptions = JSON.parse(await readFile(path.join(metadataRunArtifact, 'wp-codebox-phpunit-recipe-options.json'), 'utf8'));
  const metadataProvenance = JSON.parse(await readFile(path.join(metadataRunArtifact, 'wp-codebox-phpunit-provenance.json'), 'utf8'));
  assert.deepEqual(metadataOptions.extra_plugins.slice(1), [{ source: dataMachine, slug: 'data-machine', activate: true }]);
  assert.deepEqual(metadataProvenance.source_refs.slice(1), [{ slug: 'data-machine', source: dataMachine }]);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit aggregate smoke passed.');
