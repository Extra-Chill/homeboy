'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-bootstrap-files-'));

try {
  const extensionPath = path.join(__dirname, '..');
  const componentPath = path.join(root, 'bootstrap-fixture');
  const compatDir = path.join(componentPath, 'lib', 'compat', 'wordpress-7.0');
  const benchDir = path.join(componentPath, 'tests', 'bench');
  fs.mkdirSync(compatDir, { recursive: true });
  fs.mkdirSync(benchDir, { recursive: true });
  fs.writeFileSync(path.join(componentPath, 'bootstrap-fixture.php'), "<?php\n/* Plugin Name: Bootstrap Fixture */\n");
  fs.writeFileSync(path.join(compatDir, 'collaboration.php'), "<?php\n$GLOBALS['homeboy_bootstrap_fixture'] = true;\n");
  fs.writeFileSync(path.join(benchDir, 'noop.php'), "<?php\nreturn static fn() => ['metrics' => ['noop' => 1]];\n");

  const resultsFile = path.join(root, 'bench-results.json');
  const captureFile = path.join(root, 'captured-recipe.json');
  const fakeWpCodebox = path.join(root, 'fixture-wp-codebox.js');
  fs.writeFileSync(fakeWpCodebox, `#!/usr/bin/env node
const fs = require('node:fs');
const recipeIndex = process.argv.indexOf('--recipe');
if (process.argv[2] !== 'recipe-run' || recipeIndex < 0) {
  process.exit(2);
}
const recipe = JSON.parse(fs.readFileSync(process.argv[recipeIndex + 1], 'utf8'));
fs.writeFileSync(process.env.HOMEBOY_CAPTURE_RECIPE, JSON.stringify(recipe, null, 2) + '\\n');
process.stdout.write(JSON.stringify({
  success: true,
  benchResults: {
    component_id: 'bootstrap-fixture',
    iterations: 1,
    warmup_iterations: 0,
    scenarios: []
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
  const resolveContextHelper = path.join(root, 'resolve-context-helper.sh');
  fs.writeFileSync(resolveContextHelper, `#!/usr/bin/env bash
homeboy_resolve_context() {
  PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
  COMPONENT_ID="$HOMEBOY_COMPONENT_ID"
}
`);

  const settings = {
    wp_codebox_bootstrap_files: [
      'lib/compat/wordpress-7.1/collaboration.php',
      'lib/compat/wordpress-7.0/collaboration.php',
    ],
  };
  const result = spawnSync('bash', [path.join(extensionPath, 'scripts', 'bench', 'bench-runner.sh')], {
    cwd: componentPath,
    encoding: 'utf8',
    env: {
      ...process.env,
      HOMEBOY_BENCH_RESULTS_FILE: resultsFile,
      HOMEBOY_BENCH_ITERATIONS: '1',
      HOMEBOY_BENCH_WARMUP_ITERATIONS: '0',
      HOMEBOY_CAPTURE_RECIPE: captureFile,
      HOMEBOY_COMPONENT_ID: 'bootstrap-fixture',
      HOMEBOY_COMPONENT_PATH: componentPath,
      HOMEBOY_EXTENSION_PATH: extensionPath,
      HOMEBOY_RUNTIME_BASH_PREFLIGHT: preflightHelper,
      HOMEBOY_RUNTIME_BENCH_HELPER_SH: benchHelper,
      HOMEBOY_RUNTIME_RESOLVE_CONTEXT: resolveContextHelper,
      HOMEBOY_SETTINGS_JSON: JSON.stringify(settings),
      HOMEBOY_WP_CODEBOX_BIN: fakeWpCodebox,
    },
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  const recipe = JSON.parse(fs.readFileSync(captureFile, 'utf8'));
  const args = recipe.workflow.steps[0].args;
  const bootstrapArg = args.find((arg) => arg.startsWith('bootstrap-files-json='));
  assert.ok(bootstrapArg, 'expected bootstrap-files-json argument');
  assert.deepEqual(
    JSON.parse(bootstrapArg.slice('bootstrap-files-json='.length)),
    ['lib/compat/wordpress-7.0/collaboration.php']
  );
  assert.ok(!bootstrapArg.includes('wordpress-7.1'), 'missing fallback should not be passed');

  console.log('WP Codebox bench bootstrap files smoke passed');
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}
