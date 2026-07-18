'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-phpunit-source-root-'));

try {
  const extensionSource = path.join(__dirname, '..');
  const extensionPath = path.join(root, 'wordpress-extension');
  fs.cpSync(extensionSource, extensionPath, {
    recursive: true,
    filter: (source) => !source.includes(`${path.sep}node_modules${path.sep}`),
  });
  fs.mkdirSync(path.join(extensionPath, 'vendor'), { recursive: true });

  const componentPath = path.join(root, 'isolated-snapshot', 'phpunit-source-root-fixture');
  const sourceRoot = path.join(root, 'monorepo');
  const sourceSubpath = path.join('plugins', 'phpunit-source-root-fixture');
  const sourcePluginPath = path.join(sourceRoot, sourceSubpath);
  const generatedPath = path.join(sourcePluginPath, 'includes', 'react-admin', 'feature-config.php');
  fs.mkdirSync(path.join(sourcePluginPath, 'tests'), { recursive: true });
  fs.mkdirSync(componentPath, { recursive: true });
  fs.mkdirSync(path.join(sourceRoot, 'packages', 'php', 'monorepo-plugin'), { recursive: true });
  fs.writeFileSync(path.join(sourceRoot, 'packages', 'php', 'monorepo-plugin', 'composer.json'), '{}\n');
  fs.writeFileSync(path.join(sourcePluginPath, 'phpunit-source-root-fixture.php'), "<?php\n/* Plugin Name: PHPUnit Source Root Fixture */\n");
  fs.writeFileSync(path.join(sourcePluginPath, 'tests', 'ExampleTest.php'), "<?php\nfinal class ExampleTest extends WP_UnitTestCase {}\n");

  const prepareScript = path.join(sourcePluginPath, 'bin', 'generate-feature-config.php');
  fs.mkdirSync(path.dirname(prepareScript), { recursive: true });
  fs.writeFileSync(prepareScript, `#!/usr/bin/env php
<?php
$target = __DIR__ . '/../includes/react-admin/feature-config.php';
if (!is_dir(dirname($target))) {
    mkdir(dirname($target), 0777, true);
}
file_put_contents($target, "<?php return ['prepared' => true];\n");
`);
  fs.chmodSync(prepareScript, 0o755);

  const fakeRecipeBuilder = path.join(root, 'fake-phpunit-recipe-builder.js');
  fs.writeFileSync(fakeRecipeBuilder, `#!/usr/bin/env node
const fs = require('node:fs');
const options = JSON.parse(fs.readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify({
  schema: 'wp-codebox/workspace-recipe/v1',
  inputs: {
    extra_plugins: options.extra_plugins || [],
    mounts: options.mounts || [],
  },
  workflow: { steps: [{ command: 'wordpress.phpunit', args: [] }] },
}, null, 2) + '\\n');
`);
  fs.chmodSync(fakeRecipeBuilder, 0o755);

  const fakeWpCodebox = path.join(root, 'fixture-wp-codebox.js');
  fs.writeFileSync(fakeWpCodebox, `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const recipeIndex = process.argv.indexOf('--recipe');
if (process.argv[2] !== 'recipe-run' || recipeIndex < 0) {
  process.exit(2);
}
const recipe = JSON.parse(fs.readFileSync(process.argv[recipeIndex + 1], 'utf8'));
const extraPlugins = recipe.inputs.extra_plugins || [];
const plugin = extraPlugins.find((entry) => entry.slug === 'phpunit-source-root-fixture');
if (!plugin || plugin.source !== plugin.sourceRoot || plugin.sourceSubpath !== 'plugins/phpunit-source-root-fixture') {
  process.stderr.write('monorepo source root/subpath missing from phpunit plugin recipe input\\n');
  process.exit(10);
}
if (!fs.existsSync(path.join(plugin.source, 'packages', 'php', 'monorepo-plugin', 'composer.json'))) {
  process.stderr.write('monorepo composer path repository missing from phpunit source context\\n');
  process.exit(11);
}
if (!fs.existsSync(path.join(plugin.source, plugin.sourceSubpath, 'includes', 'react-admin', 'feature-config.php'))) {
  process.stderr.write('generated feature config missing before wp-codebox launch\\n');
  process.exit(12);
}
fs.writeFileSync(path.join(plugin.source, plugin.sourceSubpath, '.pg-test-result.txt'), 'STAGE_BEGIN:run_tests\\nSTAGE_OK:run_tests\\n');
process.stdout.write(JSON.stringify({
  success: true,
  executions: [{ stdout: 'OK (1 test, 1 assertion)\\n' }],
}) + '\\n');
`);
  fs.chmodSync(fakeWpCodebox, 0o755);

  const resolveContextHelper = path.join(root, 'resolve-context-helper.sh');
  fs.writeFileSync(resolveContextHelper, `#!/usr/bin/env bash
homeboy_resolve_context() {
  PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
  COMPONENT_ID="$HOMEBOY_COMPONENT_ID"
  EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
`);
  fs.chmodSync(resolveContextHelper, 0o755);

  const result = spawnSync('bash', [path.join(extensionPath, 'scripts', 'test', 'test-runner-wp-codebox.sh'), 'tests/ExampleTest.php'], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...process.env,
      HOMEBOY_COMPONENT_ID: 'phpunit-source-root-fixture',
      HOMEBOY_COMPONENT_PATH: componentPath,
      HOMEBOY_EXTENSION_PATH: extensionPath,
      HOMEBOY_RUNTIME_RESOLVE_CONTEXT: resolveContextHelper,
      HOMEBOY_RUNTIME_RUNNER_STEPS: path.join(root, 'missing-runner-steps.sh'),
      HOMEBOY_WP_CODEBOX_BIN: fakeWpCodebox,
      HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER: fakeRecipeBuilder,
      HOMEBOY_SETTINGS_JSON: JSON.stringify({
        wp_codebox_source_root: sourceRoot,
        wp_codebox_source_subpath: sourceSubpath,
        wp_codebox_prepare_steps: [
          { command: 'php', args: ['bin/generate-feature-config.php'] },
        ],
      }),
    },
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.equal(fs.existsSync(generatedPath), true);
  assert.match(result.stdout, /WP Codebox test run complete/);

  console.log('WP Codebox PHPUnit source root smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
