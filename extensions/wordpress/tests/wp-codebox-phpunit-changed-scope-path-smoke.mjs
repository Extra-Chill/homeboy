/**
 * Regression: Extra-Chill/homeboy#12023. The changed-file scope handed to WP
 * Codebox must be expressed in sandbox-absolute form.
 *
 * WP Codebox normalizes both the requested scope and the discovered test files
 * against the same root, and that root is the PHPUnit test root. Its relative
 * helper strips that root prefix; its only fallback looks for a literal
 * '/tests/', which a component-relative `tests/Unit/FooTest.php` does not
 * contain because it has no leading slash. A discovered
 * `/wordpress/wp-content/plugins/<slug>/tests/Unit/FooTest.php` therefore
 * normalizes to `Unit/FooTest.php` while the request normalizes to itself, and
 * the filter reports `requested=N matched=0` on every changed-scope run.
 *
 * This smoke captures the recipe options the adapter builds and asserts the
 * scope is translated, including the wp_codebox_source_subpath and custom
 * wp_codebox_phpunit_mounts cases.
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
const root = await mkdtemp(path.join(os.tmpdir(), 'wp-codebox-phpunit-changed-scope-'));

// The stub captures the options the adapter builds, then stops the run. Only
// the scope translation is under test here.
const cli = path.join(root, 'wp-codebox');
await writeFile(cli, `#!/usr/bin/env node
import fs from 'node:fs';
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args.slice(-3).join(' ') === 'runtime descriptor --json') { process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } })); process.exit(0); }
if (args[0] === 'recipe' && args[1] === 'build') {
  const optionsPath = args[args.indexOf('--options') + 1];
  fs.copyFileSync(optionsPath, process.env.CAPTURED_OPTIONS);
  fs.writeFileSync(args[args.indexOf('--output') + 1], '{"schema":"wp-codebox/workspace-recipe/v1"}');
  process.exit(0);
}
process.stdout.write('{"success":false,"executions":[]}\\n');
process.exitCode = 1;
`);
await chmod(cli, 0o755);

async function capturedScope({ name, componentLayout, settings, changed }) {
  const component = path.join(root, name);
  const testsDirectory = path.join(component, componentLayout);
  await mkdir(path.join(testsDirectory, 'tests/Unit/Deep'), { recursive: true });
  await writeFile(path.join(testsDirectory, 'sample-plugin.php'), '<?php\n/* Plugin Name: Sample Plugin */\n');
  await writeFile(path.join(testsDirectory, 'tests/Unit/Deep/OwnershipTest.php'), '<?php\n');

  const capturedOptions = path.join(root, `${name}-options.json`);
  const run = spawnSync('bash', [runner], {
    env: {
      ...process.env,
      CAPTURED_OPTIONS: capturedOptions,
      HOMEBOY_COMPONENT_PATH: component,
      COMPONENT_ID: 'sample-plugin',
      HOMEBOY_WP_CODEBOX_BIN: cli,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: path.join(root, `${name}-artifacts`),
      HOMEBOY_SETTINGS_JSON: JSON.stringify(settings),
      HOMEBOY_WORDPRESS_PHPUNIT_CHANGED_TEST_FILES: changed,
    },
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
  });
  const options = JSON.parse(await readFile(capturedOptions, 'utf8'));
  return { options, run };
}

try {
  // The reported shape: plugin at the component root, default test root.
  const plain = await capturedScope({
    name: 'plain',
    componentLayout: '.',
    settings: {},
    changed: 'tests/Unit/Deep/OwnershipTest.php',
  });
  assert.deepEqual(
    plain.options.changedTestFiles,
    ['/wordpress/wp-content/plugins/sample-plugin/tests/Unit/Deep/OwnershipTest.php'],
    'a nested changed test must be sent as a sandbox-absolute path',
  );
  // Proves the arithmetic the upstream filter performs: stripping the test root
  // prefix from both sides now yields the same key.
  const testRoot = plain.options.testRoot;
  assert.equal(testRoot, '/wordpress/wp-content/plugins/sample-plugin/tests');
  assert.ok(
    plain.options.changedTestFiles[0].startsWith(`${testRoot}/`),
    'the sent path must sit under the configured PHPUnit test root so the shared prefix strip matches',
  );
  assert.equal(
    plain.options.changedTestFiles[0].slice(testRoot.length + 1),
    'Unit/Deep/OwnershipTest.php',
  );

  // wp_codebox_source_subpath: the component root and the mounted plugin root
  // are different directories, so the subpath prefix must come off.
  const nested = await capturedScope({
    name: 'subpath',
    componentLayout: 'plugin',
    settings: { wp_codebox_source_subpath: 'plugin' },
    changed: 'plugin/tests/Unit/Deep/OwnershipTest.php',
  });
  assert.deepEqual(
    nested.options.changedTestFiles,
    ['/wordpress/wp-content/plugins/sample-plugin/tests/Unit/Deep/OwnershipTest.php'],
    'the source subpath prefix must be replaced by the sandbox plugin root, not appended to it',
  );

  // A configured mount wins over the implicit plugin mount.
  const mountedComponent = path.join(root, 'mounted');
  const mounted = await capturedScope({
    name: 'mounted',
    componentLayout: '.',
    settings: {
      wp_codebox_phpunit_mounts: [{ source: mountedComponent, target: '/home/example/public_html', mode: 'readwrite' }],
      wp_codebox_phpunit_test_root: '/home/example/public_html/tests',
    },
    changed: 'tests/Unit/Deep/OwnershipTest.php',
  });
  assert.deepEqual(
    mounted.options.changedTestFiles,
    ['/home/example/public_html/tests/Unit/Deep/OwnershipTest.php'],
    'a configured mount must define the sandbox location',
  );
  assert.ok(mounted.options.changedTestFiles[0].startsWith(`${mounted.options.testRoot}/`));

  // A path outside every mount cannot be translated. It must still be sent:
  // dropping it would either skip a selected test or empty the scope, and WP
  // Codebox reads an empty scope as "run everything".
  const outside = await capturedScope({
    name: 'outside',
    componentLayout: '.',
    settings: {},
    changed: '../elsewhere/tests/StrayTest.php',
  });
  assert.deepEqual(outside.options.changedTestFiles, ['../elsewhere/tests/StrayTest.php']);
  assert.notEqual(outside.options.changedTestFiles.length, 0, 'an untranslatable path must not empty the scope');
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log('WP Codebox PHPUnit changed-scope path translation smoke passed.');
