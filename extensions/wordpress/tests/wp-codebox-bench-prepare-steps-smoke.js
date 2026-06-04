'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-prepare-steps-'));

try {
  const extensionPath = path.join(__dirname, '..');
  const componentPath = path.join(root, 'prepare-steps-fixture');
  const benchDir = path.join(componentPath, 'tests', 'bench');
  const generatedPath = path.join(componentPath, 'includes', 'react-admin', 'feature-config.php');
  fs.mkdirSync(benchDir, { recursive: true });
  fs.writeFileSync(path.join(componentPath, 'prepare-steps-fixture.php'), "<?php\n/* Plugin Name: Prepare Steps Fixture */\n");
  fs.writeFileSync(path.join(benchDir, 'assert-prepare.php'), "<?php\nreturn static fn() => ['metrics' => ['prepared' => 1]];\n");

  const fakeWpCodebox = path.join(root, 'fixture-wp-codebox.js');
  fs.writeFileSync(fakeWpCodebox, `#!/usr/bin/env node
const fs = require('node:fs');
const recipeIndex = process.argv.indexOf('--recipe');
if (process.argv[2] !== 'recipe-run' || recipeIndex < 0) {
  process.exit(2);
}
const recipe = JSON.parse(fs.readFileSync(process.argv[recipeIndex + 1], 'utf8'));
const plugin = recipe.inputs.extraPlugins.find((entry) => entry.slug === 'prepare-steps-fixture');
if (!plugin || !fs.existsSync(plugin.source + '/includes/react-admin/feature-config.php')) {
  process.stderr.write('generated feature config missing before wp-codebox launch\\n');
  process.exit(8);
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
  const prepareScript = path.join(componentPath, 'bin', 'generate-feature-config.php');
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
    HOMEBOY_WP_CODEBOX_BIN: fakeWpCodebox,
  };

  const successResult = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...baseEnv,
      HOMEBOY_BENCH_RESULTS_FILE: path.join(root, 'success-results.json'),
      HOMEBOY_SETTINGS_JSON: JSON.stringify({
        wp_codebox_prepare_steps: [
          { command: 'php', args: ['bin/generate-feature-config.php'] },
        ],
      }),
    },
  });

  assert.equal(successResult.status, 0, successResult.stderr || successResult.stdout);
  assert.equal(fs.existsSync(generatedPath), true);

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
