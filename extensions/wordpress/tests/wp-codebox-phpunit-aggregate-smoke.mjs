/**
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';

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
await writeFile(path.join(component, 'tests', 'bootstrap.php'), `<?php
if (class_exists('WP_Post', false)) {
    throw new RuntimeException('WordPress was loaded before the project bootstrap');
}
if (!class_exists('WP_Post', false)) {
    class WP_Post {}
}
if (!function_exists('add_action')) {
    function add_action() { return true; }
}
`);
await mkdir(path.join(component, 'tests', 'nested'));
await writeFile(path.join(component, 'tests', 'FirstTest.php'), `<?php
use PHPUnit\\Framework\\TestCase;
final class FirstTest extends TestCase {
    public function test_project_stub_is_loaded(): void { $this->assertInstanceOf(WP_Post::class, new WP_Post()); }
}
`);
await writeFile(path.join(component, 'tests', 'nested', 'SecondTest.php'), `<?php
use PHPUnit\\Framework\\TestCase;
final class SecondTest extends TestCase {
    public function test_nested_xml_test_is_discovered(): void { $this->assertTrue(add_action()); }
}
`);
await writeFile(path.join(component, 'tests', 'override.php'), '<?php require_once __DIR__ . "/bootstrap.php";\n');
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
const fixture = JSON.parse(process.env.FIXTURE || '{}');
if (!fixture.crash) {
  fs.mkdirSync(artifacts + '/runtime-fixture/files', { recursive: true });
  fs.writeFileSync(artifacts + '/latest-runtime.json', fixture.pointer === 'malformed' ? '{}' : JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
  if (fixture.sidecar === 'malformed') {
    fs.writeFileSync(artifacts + '/runtime-fixture/files/test-results.json', '{');
  } else if (fixture.sidecar) {
    fs.writeFileSync(artifacts + '/runtime-fixture/files/test-results.json', JSON.stringify(fixture.sidecar));
  }
}
process.stdout.write(JSON.stringify({ executions: [{ stdout: fixture.output || '', stderr: '' }] }));
process.exitCode = fixture.exitCode || 0;
`);
await chmod(cli, 0o755);

try {
  const nativeSuite = spawnSync(path.join(extension, 'vendor', 'bin', 'phpunit'), ['--configuration', path.join(component, 'phpunit.xml.dist')], { encoding: 'utf8' });
  assert.equal(nativeSuite.status, 0, nativeSuite.stderr || nativeSuite.stdout);
  assert.match(nativeSuite.stdout, /OK \(2 tests, 2 assertions\)/);

  const unknownSidecar = { schema: 'wp-codebox/test-results/v1', status: 'unknown', summary: { total: 0, passed: 0, failed: 0, skipped: 0 } };
  const tenPassingSidecar = { schema: 'wp-codebox/test-results/v1', status: 'passed', summary: { total: 10, passed: 10, failed: 0, skipped: 0 }, suites: [], rawLogReferences: [] };
  const passedSidecar = { schema: 'wp-codebox/test-results/v1', status: 'passed', summary: { total: 3, passed: 3, failed: 0, skipped: 0 }, suites: [], rawLogReferences: [] };
  const failedSidecar = { schema: 'wp-codebox/test-results/v1', status: 'failed', summary: { total: 3, passed: 3, failed: 0, skipped: 0 } };
  const green = 'OK (3 tests, 76 assertions)\n';
  const tenGreen = 'OK (10 tests, 10 assertions)\n';
  const failures = 'ERRORS!\nTests: 281, Assertions: 329, Errors: 46, Failures: 100.\n';
  const testCases = [
    { name: 'unknown-green', fixture: { sidecar: unknownSidecar, output: green }, status: 0, artifactStatus: 'passed', expected: { total: 3, passed: 3, failed: 0, skipped: 0 } },
    { name: 'ten-passing-tests', fixture: { sidecar: tenPassingSidecar, output: tenGreen }, status: 0, artifactStatus: 'passed', expected: { total: 10, passed: 10, failed: 0, skipped: 0 } },
    { name: 'failed-green', fixture: { sidecar: failedSidecar, output: green }, status: 1, artifactStatus: 'failed', expected: { total: 3, passed: 3, failed: 0, skipped: 0 } },
    { name: 'nonzero-green', fixture: { sidecar: unknownSidecar, output: green, exitCode: 2 }, status: 2, artifactStatus: 'failed', expected: { total: 3, passed: 3, failed: 0, skipped: 0 } },
    { name: 'zero-tests', fixture: { sidecar: unknownSidecar, output: 'OK (0 tests, 0 assertions)\n' }, settings: { phpunit_no_tests: 'fail' }, status: 1, artifactStatus: 'failed', expected: { total: 0, passed: 0, failed: 0, skipped: 0 } },
    { name: 'zero-tests-skipped', fixture: { sidecar: unknownSidecar, output: 'OK (0 tests, 0 assertions)\n' }, settings: { phpunit_no_tests: 'skipped' }, status: 0, artifactStatus: 'skipped', expected: { total: 0, passed: 0, failed: 0, skipped: 0 } },
    { name: 'malformed-sidecar', fixture: { sidecar: 'malformed', output: green }, status: 1, artifactStatus: 'unknown', expected: { total: 3, passed: 3, failed: 0, skipped: 0 } },
    { name: 'missing-sidecar', fixture: { output: green }, status: 1, artifactStatus: 'unknown', expected: { total: 3, passed: 3, failed: 0, skipped: 0 } },
    { name: 'malformed-pointer', fixture: { pointer: 'malformed', output: green }, status: 1, artifactStatus: 'unknown', expected: { total: 3, passed: 3, failed: 0, skipped: 0 } },
    { name: 'errors-and-failures', fixture: { sidecar: unknownSidecar, output: failures, exitCode: 2 }, status: 2, artifactStatus: 'failed', expected: { total: 281, passed: 135, failed: 146, skipped: 0 } },
    { name: 'managed-passed', fixture: { sidecar: passedSidecar }, settings: { wp_codebox_phpunit_bootstrap_mode: 'managed' }, status: 0, artifactStatus: 'passed', expected: { total: 3, passed: 3, failed: 0, skipped: 0 } },
    { name: 'project-bootstrap-from-xml', fixture: { sidecar: passedSidecar }, settings: { wp_codebox_phpunit_bootstrap_mode: 'project' }, status: 0, artifactStatus: 'passed', expected: { total: 3, passed: 3, failed: 0, skipped: 0 } },
    { name: 'project-bootstrap-override', fixture: { sidecar: passedSidecar }, settings: { wp_codebox_phpunit_bootstrap_mode: 'project', wp_codebox_phpunit_project_bootstrap: 'tests/override.php' }, status: 0, artifactStatus: 'passed', expected: { total: 3, passed: 3, failed: 0, skipped: 0 }, projectBootstrap: 'tests/override.php' },
    { name: 'auto-without-bootstrap', fixture: { sidecar: passedSidecar }, status: 0, artifactStatus: 'passed', expected: { total: 3, passed: 3, failed: 0, skipped: 0 }, noBootstrap: true, bootstrapMode: 'managed' },
    { name: 'crash', fixture: { crash: true, output: failures, exitCode: 2 }, status: 2, artifactStatus: 'failed', expected: { total: 281, passed: 135, failed: 146, skipped: 0 } },
  ];
  for (const testCase of testCases) {
    await writeFile(path.join(component, 'phpunit.xml.dist'), testCase.noBootstrap
      ? '<phpunit><testsuites><testsuite name="suite"><directory>tests</directory></testsuite></testsuites></phpunit>\n'
      : '<phpunit bootstrap="tests/bootstrap.php"><testsuites><testsuite name="suite"><directory>tests</directory></testsuite></testsuites></phpunit>\n');
    const artifacts = path.join(root, `${testCase.name}-artifacts`);
    const results = path.join(root, `${testCase.name}-results.json`);
    const invocationArtifacts = path.join(root, `${testCase.name}-invocation-artifacts`);
    const controllerRun = path.join(root, `${testCase.name}-controller-run`);
    await mkdir(invocationArtifacts, { recursive: true });
    await mkdir(controllerRun, { recursive: true });
    await writeFile(path.join(invocationArtifacts, 'homeboy-artifact-manifest.json'), '{"schema":"homeboy/artifact-manifest/v1"}\n');
    const run = spawnSync(runner, [], { env: { ...process.env, FIXTURE: JSON.stringify(testCase.fixture), HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts, HOMEBOY_INVOCATION_ARTIFACT_DIR: invocationArtifacts, HOMEBOY_RUN_DIR: controllerRun, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter, HOMEBOY_TEST_RESULTS_FILE: results, HOMEBOY_SETTINGS_JSON: JSON.stringify({ validation_dependencies: [dependency], ...testCase.settings }) }, encoding: 'utf8' });
    assert.equal(run.status, testCase.status, `${testCase.name}: ${run.stderr}`);
    assert.deepEqual(JSON.parse(await readFile(results, 'utf8')), testCase.expected, testCase.name);
    const runArtifact = path.join(artifacts, (await readdir(artifacts)).find((entry) => entry.startsWith('wp-codebox-phpunit.')));
    const artifactDirectory = testCase.fixture.crash || testCase.fixture.pointer === 'malformed' ? runArtifact : path.join(runArtifact, 'runtime-fixture');
    const artifactResults = JSON.parse(await readFile(path.join(artifactDirectory, 'files', 'test-results.json'), 'utf8'));
    assert.equal(artifactResults.status, testCase.artifactStatus, testCase.name);
    const manifest = JSON.parse(await readFile(path.join(invocationArtifacts, 'homeboy-artifact-manifest.json'), 'utf8'));
    assert.deepEqual(manifest.artifacts.map(({ path: artifactPath }) => artifactPath), [
      'wp-codebox-phpunit/files/test-results.json',
      'wp-codebox-phpunit/files/phpunit-output.log',
      'wp-codebox-phpunit/files/phpunit-execution-diagnosis.json',
      'wp-codebox-phpunit/files/recipe-run-steps.json',
      'wp-codebox-phpunit/files/test-failures.json',
    ], testCase.name);
    const durableSummary = JSON.parse(await readFile(path.join(invocationArtifacts, 'wp-codebox-phpunit/files/test-results.json'), 'utf8')).summary;
    const controllerResults = JSON.parse(await readFile(path.join(controllerRun, 'files/test-results.json'), 'utf8'));
    assert.deepEqual(
      Object.fromEntries(['total', 'passed', 'failed', 'skipped'].map((key) => [key, durableSummary[key]])),
      testCase.expected,
      testCase.name,
    );
    assert.equal(controllerResults.status, testCase.artifactStatus, testCase.name);
    assert.equal(
      await readFile(path.join(controllerRun, 'files/phpunit-output.log'), 'utf8'),
      await readFile(path.join(invocationArtifacts, 'wp-codebox-phpunit/files/phpunit-output.log'), 'utf8'),
      testCase.name,
    );
    assert.deepEqual(
      JSON.parse(await readFile(path.join(controllerRun, 'files/phpunit-execution-diagnosis.json'), 'utf8')),
      JSON.parse(await readFile(path.join(invocationArtifacts, 'wp-codebox-phpunit/files/phpunit-execution-diagnosis.json'), 'utf8')),
      testCase.name,
    );
    const options = JSON.parse(await readFile(path.join(runArtifact, 'wp-codebox-phpunit-recipe-options.json'), 'utf8'));
    const profile = JSON.parse(await readFile(path.join(runArtifact, 'wp-codebox-phpunit-profile.json'), 'utf8'));
    const provenance = JSON.parse(await readFile(path.join(runArtifact, 'wp-codebox-phpunit-provenance.json'), 'utf8'));
    assert.deepEqual(options.extra_plugins.map(({ slug, activate }) => [slug, activate]), [
      ['dependency', true],
      ['component', true],
    ], `${testCase.name}: validation dependencies activate before the plugin under review`);
    assert.equal(options.phpunitXml, '/wordpress/wp-content/plugins/component/phpunit.xml.dist');
    assert.equal(options.autoloadFile, '/wp-codebox-vendor/autoload.php');
    assert.equal(options.bootstrapMode, testCase.bootstrapMode || (testCase.settings?.wp_codebox_phpunit_bootstrap_mode === 'managed' ? 'managed' : 'project'));
    assert.equal(options.projectBootstrap || '', testCase.projectBootstrap || '');
    assert.equal(profile.phpunit.environment, testCase.noBootstrap ? 'wordpress-integration' : 'standalone-php');
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
      FIXTURE: JSON.stringify({ sidecar: unknownSidecar, output: green }),
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
  assert.deepEqual(metadataOptions.extra_plugins, [
    { source: dataMachine, slug: 'data-machine', activate: true, composer: 'install' },
    { source: component, slug: 'component', activate: true },
  ]);
  assert.deepEqual(metadataProvenance.source_refs.slice(1), [{ slug: 'data-machine', source: dataMachine }]);

  const malformedManifestArtifacts = path.join(root, 'malformed-manifest-artifacts');
  await mkdir(malformedManifestArtifacts);
  await writeFile(path.join(malformedManifestArtifacts, 'homeboy-artifact-manifest.json'), '{"schema":"homeboy/artifact-manifest/v1","artifacts":{}}\n');
  const malformedManifestRun = spawnSync(runner, [], { env: { ...process.env, FIXTURE: JSON.stringify({ sidecar: unknownSidecar, output: green }), HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, 'malformed-manifest-run'), HOMEBOY_INVOCATION_ARTIFACT_DIR: malformedManifestArtifacts, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter, HOMEBOY_TEST_RESULTS_FILE: path.join(root, 'malformed-manifest-results.json'), HOMEBOY_SETTINGS_JSON: '{}' }, encoding: 'utf8' });
  assert.notEqual(malformedManifestRun.status, 0);
  assert.match(malformedManifestRun.stderr, /invalid artifact manifest/);

  for (const symlinkCase of ['root', 'directory', 'manifest']) {
    const invocationRoot = path.join(root, `symlink-${symlinkCase}`);
    const target = path.join(root, `symlink-${symlinkCase}-target`);
    await mkdir(target, { recursive: true });
    if (symlinkCase === 'root') {
      await symlink(target, invocationRoot);
    } else {
      await mkdir(invocationRoot);
      await writeFile(path.join(invocationRoot, 'homeboy-artifact-manifest.json'), '{"schema":"homeboy/artifact-manifest/v1"}\n');
      if (symlinkCase === 'directory') {
        await symlink(target, path.join(invocationRoot, 'wp-codebox-phpunit'));
      } else {
        await rm(path.join(invocationRoot, 'homeboy-artifact-manifest.json'));
        await symlink(path.join(target, 'manifest.json'), path.join(invocationRoot, 'homeboy-artifact-manifest.json'));
      }
    }
    const rejected = spawnSync(runner, [], { env: { ...process.env, FIXTURE: JSON.stringify({ sidecar: unknownSidecar, output: green }), HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, `symlink-${symlinkCase}-run`), HOMEBOY_INVOCATION_ARTIFACT_DIR: invocationRoot, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter, HOMEBOY_TEST_RESULTS_FILE: path.join(root, `symlink-${symlinkCase}-results.json`), HOMEBOY_SETTINGS_JSON: '{}' }, encoding: 'utf8' });
    assert.notEqual(rejected.status, 0, symlinkCase);
    assert.match(rejected.stderr, /non-symlink|symlink/, symlinkCase);
  }

  const parserFailureInvocation = path.join(root, 'parser-failure-invocation');
  const parserFailureBin = path.join(root, 'parser-failure-bin');
  await mkdir(parserFailureInvocation);
  await mkdir(parserFailureBin);
  await writeFile(path.join(parserFailureInvocation, 'homeboy-artifact-manifest.json'), '{"schema":"homeboy/artifact-manifest/v1"}\n');
  await writeFile(path.join(parserFailureBin, 'php'), '#!/usr/bin/env sh\nexit 1\n');
  await chmod(path.join(parserFailureBin, 'php'), 0o755);
  const parserFailureRun = spawnSync(runner, [], { env: { ...process.env, PATH: `${parserFailureBin}:${process.env.PATH}`, FIXTURE: JSON.stringify({ sidecar: unknownSidecar, output: green }), HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, 'parser-failure-run'), HOMEBOY_INVOCATION_ARTIFACT_DIR: parserFailureInvocation, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter, HOMEBOY_TEST_RESULTS_FILE: path.join(root, 'parser-failure-results.json'), HOMEBOY_TEST_FAILURES_FILE: path.join(root, 'parser-failure-failures.json'), HOMEBOY_SETTINGS_JSON: '{}' }, encoding: 'utf8' });
  assert.notEqual(parserFailureRun.status, 0);
  const parserFailureManifest = JSON.parse(await readFile(path.join(parserFailureInvocation, 'homeboy-artifact-manifest.json'), 'utf8'));
  assert.ok(parserFailureManifest.artifacts.some((artifact) => artifact.path === 'wp-codebox-phpunit/files/test-failures.json'));
  const parserFailureDirectory = path.join(parserFailureInvocation, 'wp-codebox-phpunit/files');
  const parserFailureSidecar = JSON.parse(await readFile(path.join(parserFailureDirectory, 'test-failures.json'), 'utf8'));
  assert.equal(parserFailureSidecar.total, 3);
  assert.equal(parserFailureSidecar.passed, 3);
  assert.equal(parserFailureSidecar.failures.length, 1);
  assert.equal(parserFailureSidecar.failures[0].test_id, 'wp-codebox-phpunit-failure-parser');
  assert.equal(parserFailureSidecar.failures[0].failure_type, 'WPCodeboxFailureParserError');
  assert.equal(parserFailureSidecar.failures[0].fingerprint.length, 64);
  assert.match(parserFailureSidecar.failures[0].message, /parse-test-failures\.sh failed with exit code 1/);
  assert.match(await readFile(path.join(parserFailureDirectory, 'phpunit-output.log'), 'utf8'), /OK \(3 tests, 76 assertions\)/);
  assert.deepEqual(JSON.parse(await readFile(path.join(root, 'parser-failure-failures.json'), 'utf8')), parserFailureSidecar);

  const concurrentInvocation = path.join(root, 'concurrent-invocation');
  await mkdir(concurrentInvocation);
  await writeFile(path.join(concurrentInvocation, 'homeboy-artifact-manifest.json'), JSON.stringify({ schema: 'homeboy/artifact-manifest/v1', artifacts: [{ id: 'unrelated', path: 'unrelated.json', kind: 'proof', provenance: { producer: 'fixture' } }] }));
  await writeFile(path.join(concurrentInvocation, 'unrelated.json'), '{}\n');
  const concurrentRun = (index) => new Promise((resolve, reject) => {
    const child = spawn(runner, [], { env: { ...process.env, FIXTURE: JSON.stringify({ sidecar: unknownSidecar, output: green }), HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, `concurrent-run-${index}`), HOMEBOY_INVOCATION_ARTIFACT_DIR: concurrentInvocation, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter, HOMEBOY_TEST_RESULTS_FILE: path.join(root, `concurrent-results-${index}.json`), HOMEBOY_SETTINGS_JSON: '{}' }, stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '';
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.on('error', reject);
    child.on('close', (status) => resolve({ status, stderr }));
  });
  let atomicReaderError;
  let atomicReaderReads = 0;
  const reader = setInterval(() => {
    readFile(path.join(concurrentInvocation, 'homeboy-artifact-manifest.json'), 'utf8').then((text) => { JSON.parse(text); atomicReaderReads += 1; }).catch((error) => { atomicReaderError ||= error; });
  }, 1);
  const concurrentStatuses = await Promise.all([concurrentRun(1), concurrentRun(2)]);
  clearInterval(reader);
  assert.deepEqual(concurrentStatuses.map(({ status }) => status), [0, 0], concurrentStatuses.map(({ stderr }) => stderr).join('\n'));
  assert.ok(atomicReaderReads > 0);
  assert.equal(atomicReaderError, undefined);
  const concurrentManifest = JSON.parse(await readFile(path.join(concurrentInvocation, 'homeboy-artifact-manifest.json'), 'utf8'));
  assert.equal(concurrentManifest.artifacts.find((artifact) => artifact.id === 'unrelated').provenance.producer, 'fixture');
  assert.equal(concurrentManifest.artifacts.filter((artifact) => artifact.path === 'wp-codebox-phpunit/files/test-results.json').length, 1);

  const crashedInvocation = path.join(root, 'crashed-invocation');
  const crashedReady = path.join(root, 'crashed-lock-ready.json');
  await mkdir(crashedInvocation);
  await writeFile(path.join(crashedInvocation, 'homeboy-artifact-manifest.json'), JSON.stringify({ schema: 'homeboy/artifact-manifest/v1', artifacts: [{ id: 'unrelated', path: 'unrelated.json', kind: 'proof', provenance: { producer: 'fixture' } }] }));
  await writeFile(path.join(crashedInvocation, 'unrelated.json'), '{}\n');
  const crashedOwner = spawn(runner, [], { env: { ...process.env, FIXTURE: JSON.stringify({ sidecar: unknownSidecar, output: green }), HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, 'crashed-owner-run'), HOMEBOY_INVOCATION_ARTIFACT_DIR: crashedInvocation, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter, HOMEBOY_TEST_RESULTS_FILE: path.join(root, 'crashed-owner-results.json'), HOMEBOY_WP_CODEBOX_PUBLICATION_LOCK_READY_FILE: crashedReady, HOMEBOY_WP_CODEBOX_PUBLICATION_LOCK_HOLD_MS: '5000', HOMEBOY_SETTINGS_JSON: '{}' }, stdio: 'ignore' });
  const ownerLease = JSON.parse(await waitForFile(crashedReady));
  assert.equal(typeof ownerLease.start_token, 'string');
  assert.notEqual(ownerLease.start_token, '');
  const liveContender = spawnSync(runner, [], { env: { ...process.env, FIXTURE: JSON.stringify({ sidecar: unknownSidecar, output: green }), HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, 'live-contender-run'), HOMEBOY_INVOCATION_ARTIFACT_DIR: crashedInvocation, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter, HOMEBOY_TEST_RESULTS_FILE: path.join(root, 'live-contender-results.json'), HOMEBOY_SETTINGS_JSON: '{}' }, encoding: 'utf8' });
  assert.notEqual(liveContender.status, 0);
  assert.match(liveContender.stderr, /Timed out waiting/);
  assert.equal(JSON.parse(await readFile(path.join(crashedInvocation, '.wp-codebox-phpunit-publication.lock', 'owner.json'), 'utf8')).token, ownerLease.token);
  crashedOwner.kill('SIGKILL');
  await new Promise((resolve) => crashedOwner.once('close', resolve));

  const recoverRun = (index) => new Promise((resolve, reject) => {
    const child = spawn(runner, [], { env: { ...process.env, FIXTURE: JSON.stringify({ sidecar: unknownSidecar, output: green }), HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, `recovered-run-${index}`), HOMEBOY_INVOCATION_ARTIFACT_DIR: crashedInvocation, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter, HOMEBOY_TEST_RESULTS_FILE: path.join(root, `recovered-results-${index}.json`), HOMEBOY_SETTINGS_JSON: '{}' }, stdio: 'ignore' });
    child.on('error', reject);
    child.on('close', (status) => resolve(status));
  });
  assert.deepEqual(await Promise.all([recoverRun(1), recoverRun(2)]), [0, 0]);
  assert.deepEqual(JSON.parse(await readFile(path.join(root, 'recovered-results-1.json'), 'utf8')), { total: 3, passed: 3, failed: 0, skipped: 0 });
  const recoveredManifest = JSON.parse(await readFile(path.join(crashedInvocation, 'homeboy-artifact-manifest.json'), 'utf8'));
  assert.equal(recoveredManifest.artifacts.find((artifact) => artifact.id === 'unrelated').provenance.producer, 'fixture');
  assert.equal(recoveredManifest.artifacts.filter((artifact) => artifact.path === 'wp-codebox-phpunit/files/test-results.json').length, 1);

  const reusedPidInvocation = path.join(root, 'reused-pid-invocation');
  const reusedPidLock = path.join(reusedPidInvocation, '.wp-codebox-phpunit-publication.lock');
  await mkdir(reusedPidLock, { recursive: true });
  await writeFile(path.join(reusedPidInvocation, 'homeboy-artifact-manifest.json'), '{"schema":"homeboy/artifact-manifest/v1"}\n');
  await writeFile(path.join(reusedPidLock, 'owner.json'), JSON.stringify({ schema: 'homeboy/wp-codebox-publication-lease/v1', pid: process.pid, hostname: ownerLease.hostname, start_token: 'reused-pid-token', token: 'stale-owner' }));
  const reusedPidRun = spawnSync(runner, [], { env: { ...process.env, FIXTURE: JSON.stringify({ sidecar: unknownSidecar, output: green }), HOMEBOY_COMPONENT_PATH: component, COMPONENT_ID: 'component', HOMEBOY_WP_CODEBOX_BIN: cli, HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, 'reused-pid-run'), HOMEBOY_INVOCATION_ARTIFACT_DIR: reusedPidInvocation, HOMEBOY_RUNTIME_WRITE_TEST_RESULTS: resultsWriter, HOMEBOY_TEST_RESULTS_FILE: path.join(root, 'reused-pid-results.json'), HOMEBOY_SETTINGS_JSON: '{}' }, encoding: 'utf8' });
  assert.equal(reusedPidRun.status, 0, reusedPidRun.stderr);
  assert.deepEqual(JSON.parse(await readFile(path.join(root, 'reused-pid-results.json'), 'utf8')), { total: 3, passed: 3, failed: 0, skipped: 0 });
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit aggregate smoke passed.');

async function waitForFile(target) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try { return await readFile(target, 'utf8'); } catch (error) {
      if (error.code !== 'ENOENT') { throw error; }
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
  }
  throw new Error(`Timed out waiting for ${target}`);
}
