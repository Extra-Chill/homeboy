'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-bootstrap-steps-'));

try {
  const extensionPath = path.join(__dirname, '..');
  const componentPath = path.join(root, 'bootstrap-steps-fixture');
  const dependencyPath = path.join(root, 'generic-dependency');
  const monorepoDependencyPath = path.join(root, 'woocommerce@fix-test-branch');
  const monorepoPluginPath = path.join(monorepoDependencyPath, 'plugins', 'woocommerce');
  const stripeDependencyPath = path.join(root, 'woocommerce-gateway-stripe');
  const failingDependencyPath = path.join(root, 'gateway-build-fails');
  const fixtureCoreModule = path.join(__dirname, 'fixtures', 'wp-codebox-core-bench-runner.mjs');
  const benchDir = path.join(componentPath, 'tests', 'bench');
  const fakeBinDir = path.join(root, 'bin');
  fs.mkdirSync(benchDir, { recursive: true });
  fs.mkdirSync(dependencyPath, { recursive: true });
  fs.mkdirSync(monorepoPluginPath, { recursive: true });
  fs.mkdirSync(stripeDependencyPath, { recursive: true });
  fs.mkdirSync(failingDependencyPath, { recursive: true });
  fs.mkdirSync(fakeBinDir, { recursive: true });
  fs.writeFileSync(path.join(componentPath, 'bootstrap-steps-fixture.php'), "<?php\n/* Plugin Name: Bootstrap Steps Fixture */\n");
  fs.writeFileSync(path.join(dependencyPath, 'generic-dependency.php'), "<?php\n/* Plugin Name: Generic Dependency */\n");
  fs.writeFileSync(path.join(monorepoPluginPath, 'woocommerce.php'), "<?php\n/* Plugin Name: WooCommerce */\n");
  fs.writeFileSync(path.join(stripeDependencyPath, 'woocommerce-gateway-stripe.php'), "<?php\n/* Plugin Name: WooCommerce Stripe Gateway */\nhomeboy_missing_wordpress_runtime_function();\n");
  fs.writeFileSync(path.join(failingDependencyPath, 'gateway-build-fails.php'), "<?php\n/* Plugin Name: Gateway Build Fails */\n");
  fs.writeFileSync(path.join(failingDependencyPath, 'composer.json'), JSON.stringify({ scripts: { postInstall: 'npm install' } }, null, 2));
  fs.writeFileSync(path.join(failingDependencyPath, 'package.json'), JSON.stringify({ engines: { node: '>=99', npm: '>=99' } }, null, 2));
  fs.writeFileSync(path.join(benchDir, 'assert-bootstrap.php'), "<?php\nreturn static fn() => ['metrics' => ['bootstrap_seen' => 1]];\n");
  const fakeComposer = path.join(fakeBinDir, 'composer');
  fs.writeFileSync(fakeComposer, `#!/usr/bin/env bash
printf 'npm error code EBADENGINE\n' >&2
printf 'npm error engine Unsupported engine for woocommerce-gateway-stripe\n' >&2
exit 1
`);
  fs.chmodSync(fakeComposer, 0o755);

  const successCaptureFile = path.join(root, 'captured-success-recipe.json');
  const failureArtifactsDir = path.join(root, 'failure-artifacts');
  const fakeWpCodebox = path.join(root, 'fixture-wp-codebox.js');
  fs.writeFileSync(fakeWpCodebox, `#!/usr/bin/env node
const fs = require('node:fs');
const recipeIndex = process.argv.indexOf('--recipe');
if (process.argv[2] !== 'recipe-run' || recipeIndex < 0) {
  process.exit(2);
}
const recipe = JSON.parse(fs.readFileSync(process.argv[recipeIndex + 1], 'utf8'));
if (process.env.HOMEBOY_FORCE_BOOTSTRAP_FAILURE === '1') {
  process.stdout.write(JSON.stringify({
    success: false,
    diagnostics: [{ schema: 'wp-codebox/plugin-runtime-diagnostic/v1', phase: 'setup', message: 'fixture bootstrap failed' }]
  }) + '\\n');
  process.exit(9);
}
fs.writeFileSync(process.env.HOMEBOY_CAPTURE_RECIPE, JSON.stringify(recipe, null, 2) + '\\n');
process.stdout.write(JSON.stringify({
  success: true,
  benchResults: {
    component_id: 'bootstrap-steps-fixture',
    iterations: 1,
    warmup_iterations: 0,
    scenarios: [{ id: 'assert-bootstrap', metrics: { bootstrap_seen: 1 } }]
  }
}) + '\\n');
`);
  fs.chmodSync(fakeWpCodebox, 0o755);

  const benchHelper = path.join(root, 'bench-helper.sh');
  fs.writeFileSync(benchHelper, `#!/usr/bin/env bash
homeboy_write_empty_bench_results() {
  local component="$1"
  local iterations="$2"
  local results_file="$3"
  printf '{"component_id":"%s","iterations":%s,"scenarios":[]}\\n' "$component" "$iterations" > "$results_file"
}
`);
  const preflightHelper = path.join(root, 'preflight-helper.sh');
  fs.writeFileSync(preflightHelper, `#!/usr/bin/env bash
homeboy_require_bash_version() { :; }
`);

  const settings = {
    validation_dependencies: [dependencyPath, monorepoDependencyPath, stripeDependencyPath],
    wp_codebox_core_module: fixtureCoreModule,
    wp_codebox_extra_plugins: [
      { source: '/tmp/runtime-prerequisite', slug: 'runtime-prerequisite', activate: true },
    ],
    wp_codebox_bootstrap_steps: [
      { command: 'wordpress.wp-cli', args: ['command=option update generic_dependency_bootstrap yes'] },
    ],
  };
  const baseEnv = {
    ...process.env,
    HOMEBOY_BENCH_ITERATIONS: '1',
    HOMEBOY_BENCH_WARMUP_ITERATIONS: '0',
    HOMEBOY_COMPONENT_ID: 'bootstrap-steps-fixture',
    HOMEBOY_COMPONENT_PATH: componentPath,
    HOMEBOY_EXTENSION_PATH: extensionPath,
    HOMEBOY_RUNTIME_BASH_PREFLIGHT: preflightHelper,
    HOMEBOY_RUNTIME_BENCH_HELPER_SH: benchHelper,
    HOMEBOY_SETTINGS_JSON: JSON.stringify(settings),
    HOMEBOY_WP_CODEBOX_BIN: fakeWpCodebox,
  };

  const successResult = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...baseEnv,
      HOMEBOY_BENCH_RESULTS_FILE: path.join(root, 'success-results.json'),
      HOMEBOY_CAPTURE_RECIPE: successCaptureFile,
    },
  });

  assert.equal(successResult.status, 0, successResult.stderr || successResult.stdout);
  const recipe = JSON.parse(fs.readFileSync(successCaptureFile, 'utf8'));
  assert.equal(recipe.inputs.extra_plugins.some((plugin) => plugin.slug === 'generic-dependency'), true);
  const woocommercePlugin = recipe.inputs.extra_plugins.find((plugin) => plugin.slug === 'woocommerce');
  assert.equal(woocommercePlugin.source, monorepoPluginPath);
  assert.equal(woocommercePlugin.pluginFile, 'woocommerce/woocommerce.php');
  assert.equal(recipe.inputs.extra_plugins.some((plugin) => plugin.slug === 'woocommerce-gateway-stripe'), true);
  assert.deepEqual(recipe.inputs.extra_plugins.find((plugin) => plugin.slug === 'runtime-prerequisite'), settings.wp_codebox_extra_plugins[0]);
  assert.deepEqual(recipe.inputs.pluginRuntime.setup, settings.wp_codebox_bootstrap_steps);
  assert.equal(recipe.workflow.steps[0].command, 'wordpress.bench');

  const successResults = JSON.parse(fs.readFileSync(path.join(root, 'success-results.json'), 'utf8'));
  assert.ok(successResults.prepared_dependencies.some((dependency) => dependency.slug === 'woocommerce' && dependency.source_path === fs.realpathSync(monorepoDependencyPath) && dependency.package_root === fs.realpathSync(monorepoPluginPath) && dependency.mounted_plugin_dir === '/wordpress/wp-content/plugins/woocommerce'), JSON.stringify(successResults.prepared_dependencies, null, 2));
  assert.ok(successResults.prepared_dependencies.some((dependency) => dependency.slug === 'woocommerce-gateway-stripe' && dependency.source_path === fs.realpathSync(stripeDependencyPath) && dependency.package_root === fs.realpathSync(stripeDependencyPath)), JSON.stringify(successResults.prepared_dependencies, null, 2));

  const scopedCoreResult = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...baseEnv,
      HOMEBOY_BENCH_SCENARIOS: 'assert-bootstrap',
      HOMEBOY_BENCH_RESULTS_FILE: path.join(root, 'scoped-core-results.json'),
      HOMEBOY_CAPTURE_RECIPE: path.join(root, 'captured-scoped-core-recipe.json'),
      HOMEBOY_SETTINGS_JSON: JSON.stringify({
        ...settings,
        bench_env: { WC_CHECKOUT_GATEWAY_MATRIX_PROFILES: 'core_bacs,core_cheque,core_cod' },
        validation_dependencies: [
          dependencyPath,
          {
            dependency: failingDependencyPath,
            scenarios: ['assert-bootstrap'],
            profiles: ['plugin_stripe'],
            profile_env: 'WC_CHECKOUT_GATEWAY_MATRIX_PROFILES',
          },
        ],
      }),
      PATH: `${fakeBinDir}${path.delimiter}${process.env.PATH}`,
    },
  });

  assert.equal(scopedCoreResult.status, 0, scopedCoreResult.stderr || scopedCoreResult.stdout);
  const scopedCoreResults = JSON.parse(fs.readFileSync(path.join(root, 'scoped-core-results.json'), 'utf8'));
  assert.equal(scopedCoreResults.prepared_dependencies.some((dependency) => dependency.slug === 'generic-dependency'), true);
  assert.equal(scopedCoreResults.prepared_dependencies.some((dependency) => dependency.slug === 'gateway-build-fails'), false);
  assert.equal(scopedCoreResults.dependency_build_failures, undefined);

  const buildFailureArtifactsDir = path.join(root, 'build-failure-artifacts');
  const buildFailureResult = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...baseEnv,
      HOMEBOY_BENCH_SCENARIOS: 'assert-bootstrap',
      HOMEBOY_BENCH_RESULTS_FILE: path.join(root, 'build-failure-results.json'),
      HOMEBOY_CAPTURE_RECIPE: path.join(root, 'captured-build-failure-recipe.json'),
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: buildFailureArtifactsDir,
      HOMEBOY_SETTINGS_JSON: JSON.stringify({
        ...settings,
        bench_env: { WC_CHECKOUT_GATEWAY_MATRIX_PROFILES: 'plugin_stripe' },
        validation_dependencies: [
          dependencyPath,
          {
            dependency: failingDependencyPath,
            scenarios: ['assert-bootstrap'],
            profiles: ['plugin_stripe'],
            profile_env: 'WC_CHECKOUT_GATEWAY_MATRIX_PROFILES',
          },
        ],
      }),
      PATH: `${fakeBinDir}${path.delimiter}${process.env.PATH}`,
    },
  });

  assert.equal(buildFailureResult.status, 0, buildFailureResult.stderr || buildFailureResult.stdout);
  const buildFailureResults = JSON.parse(fs.readFileSync(path.join(root, 'build-failure-results.json'), 'utf8'));
  assert.equal(buildFailureResults.prepared_dependencies.some((dependency) => dependency.slug === 'generic-dependency'), true);
  assert.equal(buildFailureResults.prepared_dependencies.some((dependency) => dependency.slug === 'gateway-build-fails'), false);
  assert.equal(buildFailureResults.dependency_build_failures.length, 1);
  assert.equal(buildFailureResults.dependency_build_failures[0].dependency_slug, 'gateway-build-fails');
  assert.equal(path.basename(buildFailureResults.dependency_build_failures[0].dependency_path), 'gateway-build-fails');
  assert.match(buildFailureResults.dependency_build_failures[0].package_path, /gateway-build-fails/);
  assert.equal(buildFailureResults.dependency_build_failures[0].engine_requirements.node, '>=99');
  assert.match(buildFailureResults.dependency_build_failures[0].attempted_command, /composer install/);
  assert.match(buildFailureResults.dependency_build_failures[0].stderr_tail, /EBADENGINE/);
  const buildDiagnostics = JSON.parse(fs.readFileSync(path.join(buildFailureArtifactsDir, 'wordpress-dependency-build-diagnostics.json'), 'utf8'));
  assert.equal(buildDiagnostics.diagnostics[0].code, 'wordpress-bench-dependency-build-failed');

  const failureResult = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...baseEnv,
      HOMEBOY_BENCH_RESULTS_FILE: path.join(root, 'failure-results.json'),
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: failureArtifactsDir,
      HOMEBOY_FORCE_BOOTSTRAP_FAILURE: '1',
    },
  });

  assert.equal(failureResult.status, 9);
  assert.match(failureResult.stderr, /WP Codebox bench bootstrap setup failed before measured workloads executed/);
  const diagnostics = JSON.parse(fs.readFileSync(path.join(failureArtifactsDir, 'wp-codebox-bench-bootstrap-diagnostics.json'), 'utf8'));
  assert.equal(diagnostics.diagnostics[0].code, 'wp-codebox-bench-bootstrap-failed');
  assert.equal(diagnostics.diagnostics[0].phase, 'setup');

  console.log('WP Codebox bench bootstrap steps smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
