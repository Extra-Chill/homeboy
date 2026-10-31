'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-prepare-steps-'));

try {
  const extensionPath = path.join(__dirname, '..');
  const componentPath = path.join(root, 'isolated-snapshot', 'prepare-steps-fixture');
  const sourceRoot = path.join(root, 'monorepo');
  const sourceSubpath = path.join('plugins', 'prepare-steps-fixture');
  const sourcePluginPath = path.join(sourceRoot, sourceSubpath);
  const benchDir = path.join(sourcePluginPath, 'tests', 'bench');
  const generatedPath = path.join(sourcePluginPath, 'includes', 'react-admin', 'feature-config.php');
  fs.mkdirSync(benchDir, { recursive: true });
  fs.mkdirSync(componentPath, { recursive: true });
  fs.mkdirSync(path.join(sourceRoot, 'packages', 'php', 'monorepo-plugin'), { recursive: true });
  fs.writeFileSync(path.join(sourceRoot, 'packages', 'php', 'monorepo-plugin', 'composer.json'), '{}\n');
  fs.writeFileSync(path.join(sourcePluginPath, 'prepare-steps-fixture.php'), "<?php\n/* Plugin Name: Prepare Steps Fixture */\n");
  fs.writeFileSync(path.join(benchDir, 'assert-prepare.php'), "<?php\nreturn static fn() => ['metrics' => ['prepared' => 1]];\n");

  const fakeWpCodebox = path.join(root, 'fixture-wp-codebox.js');
  fs.writeFileSync(fakeWpCodebox, `#!/usr/bin/env node
const fs = require('node:fs');
const recipeIndex = process.argv.indexOf('--recipe');
if (process.argv[2] !== 'recipe-run' || recipeIndex < 0) {
  process.exit(2);
}
const recipe = JSON.parse(fs.readFileSync(process.argv[recipeIndex + 1], 'utf8'));
const extraPlugins = recipe.inputs.extra_plugins || [];
const plugin = extraPlugins.find((entry) => entry.slug === 'prepare-steps-fixture');
if (!plugin || !plugin.sourceSubpath || !fs.existsSync(plugin.source + '/' + plugin.sourceSubpath + '/includes/react-admin/feature-config.php')) {
  process.stderr.write('generated feature config missing before wp-codebox launch\\n');
  process.exit(8);
}
if (plugin.source !== plugin.sourceRoot || plugin.sourceSubpath !== 'plugins/prepare-steps-fixture') {
  process.stderr.write('monorepo source root/subpath missing from plugin recipe input\\n');
  process.exit(10);
}
if (!fs.existsSync(plugin.source + '/packages/php/monorepo-plugin/composer.json')) {
  process.stderr.write('monorepo composer path repository missing from plugin source context\\n');
  process.exit(9);
}
if (process.env.FAKE_WP_CODEBOX_FAILURE === '1') {
  process.stdout.write(JSON.stringify({
    success: false,
    failure_classification: 'assertion_failure',
    message: 'fixture workload assertion failed',
    benchResults: {
      component_id: 'prepare-steps-fixture',
      iterations: 1,
      warmup_iterations: 0,
      scenarios: [{ id: 'assert-prepare', metrics: { mean_ms: 12.5 } }]
    }
  }) + '\\n');
  process.exit(0);
}
process.stdout.write(JSON.stringify({
  success: true,
  benchResults: {
    component_id: 'prepare-steps-fixture',
    iterations: 1,
    warmup_iterations: 0,
    scenarios: [{ id: 'assert-prepare', metrics: { prepared: 1 } }]
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
  printf '{"component_id":"%s","iterations":%s,"scenarios":[]}\n' "$component" "$iterations" > "$results_file"
}
`);
  const preflightHelper = path.join(root, 'preflight-helper.sh');
  fs.writeFileSync(preflightHelper, `#!/usr/bin/env bash
homeboy_require_bash_version() { :; }
`);
  const resolveContextHelper = path.join(root, 'resolve-context-helper.sh');
  fs.writeFileSync(resolveContextHelper, `#!/usr/bin/env bash
homeboy_resolve_context() {
  PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
  COMPONENT_ID="$HOMEBOY_COMPONENT_ID"
}
`);
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

  const baseEnv = {
    ...process.env,
    HOMEBOY_BENCH_ITERATIONS: '1',
    HOMEBOY_BENCH_WARMUP_ITERATIONS: '0',
    HOMEBOY_COMPONENT_ID: 'prepare-steps-fixture',
    HOMEBOY_COMPONENT_PATH: componentPath,
    HOMEBOY_EXTENSION_PATH: extensionPath,
    HOMEBOY_RUNTIME_BASH_PREFLIGHT: preflightHelper,
    HOMEBOY_RUNTIME_BENCH_HELPER_SH: benchHelper,
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT: resolveContextHelper,
    HOMEBOY_WP_CODEBOX_BIN: fakeWpCodebox,
  };

  const successResult = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...baseEnv,
      HOMEBOY_BENCH_RESULTS_FILE: path.join(root, 'success-results.json'),
      HOMEBOY_SETTINGS_JSON: JSON.stringify({
        wp_codebox_source_root: sourceRoot,
        wp_codebox_source_subpath: sourceSubpath,
        wp_codebox_prepare_steps: [
          { command: 'php', args: ['bin/generate-feature-config.php'] },
        ],
      }),
    },
  });

  assert.equal(successResult.status, 0, successResult.stderr || successResult.stdout);
  assert.equal(fs.existsSync(generatedPath), true);

  const staleResultsPath = path.join(root, 'stale-results.json');
  fs.writeFileSync(staleResultsPath, JSON.stringify({
    component_id: 'prepare-steps-fixture',
    iterations: 0,
    scenarios: [{ id: 'assert-prepare', metrics: { browser_peak_used_js_heap_bytes: 999 } }],
  }));
  const childFailureArtifactsDir = path.join(root, 'child-failure-artifacts');
  const childFailureResult = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...baseEnv,
      FAKE_WP_CODEBOX_FAILURE: '1',
      HOMEBOY_BENCH_RESULTS_FILE: staleResultsPath,
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: childFailureArtifactsDir,
      HOMEBOY_SETTINGS_JSON: JSON.stringify({
        wp_codebox_source_root: sourceRoot,
        wp_codebox_source_subpath: sourceSubpath,
        wp_codebox_prepare_steps: [
          { command: 'php', args: ['bin/generate-feature-config.php'] },
        ],
      }),
    },
  });

  assert.equal(childFailureResult.status, 1);
  assert.match(childFailureResult.stderr, /WP Codebox wordpress\.bench did not return a successful bench result envelope/);
  assert.match(childFailureResult.stderr, /Failure classification: assertion_failure/);
  const preservedResults = JSON.parse(fs.readFileSync(staleResultsPath, 'utf8'));
  assert.equal(preservedResults.iterations, 1);
  assert.equal(preservedResults.scenarios[0].metrics.mean_ms, 12.5);
  assert.equal(preservedResults.scenarios[0].metrics.browser_peak_used_js_heap_bytes, undefined);
  const childFailureDiagnostics = JSON.parse(fs.readFileSync(path.join(childFailureArtifactsDir, 'wp-codebox-bench-run-diagnostics.json'), 'utf8'));
  assert.equal(childFailureDiagnostics.diagnostics[0].failure_classification, 'assertion_failure');
  assert.equal(childFailureDiagnostics.diagnostics[0].persisted_child_bench_results, true);

  fs.rmSync(generatedPath, { force: true });
  const failureArtifactsDir = path.join(root, 'failure-artifacts');
  const failureResult = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...baseEnv,
      HOMEBOY_BENCH_RESULTS_FILE: path.join(root, 'failure-results.json'),
      HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR: failureArtifactsDir,
      HOMEBOY_SETTINGS_JSON: JSON.stringify({
        wp_codebox_source_root: sourceRoot,
        wp_codebox_source_subpath: sourceSubpath,
        wp_codebox_prepare_steps: [
          { command: 'php', args: ['missing-generate-feature-config.php'] },
        ],
      }),
    },
  });

  assert.equal(failureResult.status, 1);
  assert.match(failureResult.stderr, /WP Codebox bench prepare step failed before plugin runtime launch/);
  const diagnostics = JSON.parse(fs.readFileSync(path.join(failureArtifactsDir, 'wp-codebox-bench-prepare-diagnostics.json'), 'utf8'));
  assert.equal(diagnostics.diagnostics[0].code, 'wp-codebox-bench-prepare-failed');
  assert.equal(diagnostics.diagnostics[0].phase, 'prepare');
  assert.equal(diagnostics.diagnostics[0].command, 'php');

  console.log('WP Codebox bench prepare steps smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
