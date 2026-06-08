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
  const fixtureCoreModule = path.join(__dirname, 'fixtures', 'wp-codebox-core-bench-runner.mjs');
  const benchDir = path.join(componentPath, 'tests', 'bench');
  fs.mkdirSync(benchDir, { recursive: true });
  fs.mkdirSync(dependencyPath, { recursive: true });
  fs.writeFileSync(path.join(componentPath, 'bootstrap-steps-fixture.php'), "<?php\n/* Plugin Name: Bootstrap Steps Fixture */\n");
  fs.writeFileSync(path.join(dependencyPath, 'generic-dependency.php'), "<?php\n/* Plugin Name: Generic Dependency */\n");
  fs.writeFileSync(path.join(benchDir, 'assert-bootstrap.php'), "<?php\nreturn static fn() => ['metrics' => ['bootstrap_seen' => 1]];\n");

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
    validation_dependencies: [dependencyPath],
    wp_codebox_core_module: fixtureCoreModule,
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
  assert.equal(recipe.inputs.extraPlugins.some((plugin) => plugin.slug === 'generic-dependency'), true);
  assert.deepEqual(recipe.inputs.pluginRuntime.setup, settings.wp_codebox_bootstrap_steps);
  assert.equal(recipe.workflow.steps[0].command, 'wordpress.bench');

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
