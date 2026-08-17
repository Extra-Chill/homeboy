/**
 * Regression: the plugin under review must be mounted, bootstrapped, and
 * activated alongside its declared validation dependencies, the selected
 * changed PHPUnit files must reach the sandbox, and a zero-test result must
 * name the seam that produced it.
 *
 * External dependencies
 */
import { strict as assert } from 'node:assert';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const extension = path.resolve(import.meta.dirname, '..');
const runner = path.join(extension, 'scripts/test/test-runner.sh');
const codeboxRunner = path.join(extension, 'scripts/test/test-runner-wp-codebox.sh');
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-target-activation-'));

const component = path.join(root, 'data-machine-socials');
const dependency = path.join(root, 'data-machine');
const cli = path.join(root, 'wp-codebox');
const recipeOptionsCapture = path.join(root, 'recipe-options.json');
const invocationArtifacts = path.join(root, 'invocation-artifacts');
const artifacts = path.join(root, 'artifacts');
const runnerPrelude = path.join(root, 'runner-prelude.sh');

await mkdir(path.join(component, 'tests/Unit/Abilities'), { recursive: true });
await mkdir(dependency, { recursive: true });
await mkdir(invocationArtifacts, { recursive: true });
await writeFile(path.join(invocationArtifacts, 'homeboy-artifact-manifest.json'), '{"schema":"homeboy/artifact-manifest/v1"}\n');
await writeFile(path.join(component, 'data-machine-socials.php'), '<?php\n/* Plugin Name: Data Machine Socials */\n');
await writeFile(path.join(dependency, 'data-machine.php'), '<?php\n/* Plugin Name: Data Machine */\n');
await writeFile(path.join(component, 'tests/Unit/Abilities/SocialCommentsAbilityTest.php'), '<?php\n// selected changed test\n');
await writeFile(path.join(component, 'tests/Unit/RestApiRecentCommentsTest.php'), '<?php\n// selected changed test\n');
await writeFile(path.join(component, 'tests/Unit/UnselectedTest.php'), '<?php\n// not part of the changed scope\n');
await writeFile(runnerPrelude, `homeboy_runner_init() {
    COMPONENT_PATH="\${HOMEBOY_COMPONENT_PATH:?HOMEBOY_COMPONENT_PATH is required}"
    PLUGIN_PATH="$COMPONENT_PATH"
    EXTENSION_PATH="\${HOMEBOY_EXTENSION_PATH:?HOMEBOY_EXTENSION_PATH is required}"
}
`);

// The stub stands in for WP Codebox: it records the recipe options the adapter
// produced and replays a sandbox that discovered nothing, which is the shape
// the zero-test diagnosis has to classify.
await writeFile(cli, `#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args[0] === 'recipe' && args[1] === 'build') {
  fs.copyFileSync(args[args.indexOf('--options') + 1], ${JSON.stringify(recipeOptionsCapture)});
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
const artifactRoot = args[args.indexOf('--artifacts') + 1];
const runtime = path.join(artifactRoot, 'runtime-fixture');
fs.mkdirSync(path.join(runtime, 'files', 'phpunit'), { recursive: true });
fs.writeFileSync(path.join(artifactRoot, 'latest-runtime.json'), JSON.stringify({ paths: { runtimeDirectory: 'runtime-fixture' } }));
fs.writeFileSync(path.join(runtime, 'files', 'phpunit', '.pg-test-result.txt'), [
  'STAGE_BEGIN:load_component',
  'PLUGIN_DETECTED data-machine-socials.php',
  'STAGE_OK:load_component',
  'PLUGIN_ACTIVATE data-machine/data-machine.php',
  'PLUGIN_ACTIVATE_OK data-machine/data-machine.php stage=activation',
  'PLUGIN_ACTIVATE data-machine-socials/data-machine-socials.php',
  'PLUGIN_ACTIVATE_OK data-machine-socials/data-machine-socials.php stage=activation',
  'SCOPED_TEST_FILES requested=2 matched=0',
  'DISCOVERY: dirs=/wordpress/wp-content/plugins/data-machine-socials/tests files=0 suffixes=Test.php prefixes=test- excludes=0 found=0',
  '',
].join('\\n'));
fs.writeFileSync(path.join(runtime, 'files', 'test-results.json'), JSON.stringify({
  schema: 'wp-codebox/test-results/v1',
  status: 'skipped',
  summary: { total: 0, passed: 0, failed: 0, skipped: 0, unknown: 0 },
  suites: [],
}));
process.stdout.write(JSON.stringify({ success: true, executions: [{ stdout: 'Tests: 0, Assertions: 0.\\n', stderr: '' }] }));
`);
await chmod(cli, 0o755);

const run = spawnSync('bash', [runner], {
  env: {
    ...process.env,
    HOMEBOY_RUNTIME_RUNNER_PRELUDE: runnerPrelude,
    HOMEBOY_EXTENSION_PATH: extension,
    HOMEBOY_COMPONENT_PATH: component,
    HOMEBOY_COMPONENT_ID: 'data-machine-socials',
    COMPONENT_ID: 'data-machine-socials',
    HOMEBOY_COMPONENT_SHAPE: 'plugin',
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX: codeboxRunner,
    HOMEBOY_WP_CODEBOX_BIN: cli,
    HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: artifacts,
    HOMEBOY_INVOCATION_ARTIFACT_DIR: invocationArtifacts,
    HOMEBOY_WORDPRESS_DEPENDENCY_PATHS: dependency,
    HOMEBOY_CHANGED_TEST_FILES: 'tests/Unit/Abilities/SocialCommentsAbilityTest.php\ntests/Unit/RestApiRecentCommentsTest.php',
  },
  encoding: 'utf8',
});

const transcript = `${run.stdout || ''}${run.stderr || ''}`;

try {
  const options = JSON.parse(await readFile(recipeOptionsCapture, 'utf8'));

  // The target must be present in the plugins WP Codebox activates, and it
  // must activate after the validation dependencies it declares.
  const activated = options.extra_plugins.filter((plugin) => plugin.activate !== false).map((plugin) => plugin.slug);
  assert.deepEqual(activated, ['data-machine', 'data-machine-socials'], `unexpected activation set: ${JSON.stringify(options.extra_plugins)}`);
  assert.equal(options.extra_plugins.every((plugin) => plugin.activate === true), true, 'every recipe plugin must be activated');

  // The selected changed PHPUnit files must reach the sandbox as a scope, in
  // sandbox-absolute form. WP Codebox normalizes the scope and the discovered
  // files against the PHPUnit test root, so a component-relative path can never
  // match a discovered one. See Extra-Chill/homeboy#12023.
  assert.deepEqual(options.changedTestFiles, [
    '/wordpress/wp-content/plugins/data-machine-socials/tests/Unit/Abilities/SocialCommentsAbilityTest.php',
    '/wordpress/wp-content/plugins/data-machine-socials/tests/Unit/RestApiRecentCommentsTest.php',
  ], `unexpected changed test scope: ${JSON.stringify(options.changedTestFiles)}`);

  const publishedFiles = path.join(invocationArtifacts, 'wp-codebox-phpunit', 'files');
  const diagnosis = JSON.parse(await readFile(path.join(publishedFiles, 'phpunit-execution-diagnosis.json'), 'utf8'));
  assert.equal(diagnosis.schema, 'homeboy/wordpress-phpunit-execution-diagnosis/v1');
  assert.equal(diagnosis.executed_tests, 0);
  assert.equal(diagnosis.cause, 'changed_file_filter_mismatch', `unexpected zero-test cause: ${diagnosis.cause}`);
  assert.equal(diagnosis.activation.target.slug, 'data-machine-socials');
  assert.equal(diagnosis.activation.target.activated, true);
  assert.deepEqual(diagnosis.activation.validation_dependencies.map((entry) => [entry.slug, entry.activated]), [['data-machine', true]]);
  assert.deepEqual(diagnosis.activation.order, [
    { role: 'validation-dependency', slug: 'data-machine' },
    { role: 'target', slug: 'data-machine-socials' },
  ]);
  // The two representations are deliberately different. An operator reading the
  // diagnosis gets repo-relative paths; the sandbox form is recorded alongside
  // so the requested-vs-matched arithmetic can be checked against it.
  assert.deepEqual(diagnosis.scope.changed_test_files, [
    'tests/Unit/Abilities/SocialCommentsAbilityTest.php',
    'tests/Unit/RestApiRecentCommentsTest.php',
  ]);
  assert.deepEqual(diagnosis.scope.changed_test_files_sandbox, options.changedTestFiles);
  assert.deepEqual(diagnosis.scope.changed_test_files_untranslated, []);

  // Raw PHPUnit output stays directly retrievable from the run evidence.
  const manifest = JSON.parse(await readFile(path.join(invocationArtifacts, 'homeboy-artifact-manifest.json'), 'utf8'));
  const registered = manifest.artifacts.map((artifact) => artifact.path);
  assert.equal(registered.includes('wp-codebox-phpunit/files/phpunit-output.log'), true, `raw output not registered: ${registered.join(', ')}`);
  assert.equal(registered.includes('wp-codebox-phpunit/files/phpunit-execution-diagnosis.json'), true, `diagnosis not registered: ${registered.join(', ')}`);
  await readFile(path.join(publishedFiles, 'phpunit-output.log'), 'utf8');

  assert.match(transcript, /PHPUNIT_ZERO_TESTS cause=changed_file_filter_mismatch/);
  assert.match(transcript, /PHPUnit execution diagnosis: artifact:\/\/files\/phpunit-execution-diagnosis\.json/);
} catch (error) {
  process.stderr.write(`${transcript}\n`);
  throw error;
} finally {
  await rm(root, { recursive: true, force: true });
}

process.stdout.write('WP Codebox PHPUnit target activation smoke passed\n');
